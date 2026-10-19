//! The rounds: mirror each mainnet feed's new round, then keep each pool at
//! its feed's price.
//!
//! What a pool needs is decided by [`decide`], from what was read alone, so
//! the rules can be checked without a chain: outside the band the pool is
//! taken back to the feed's price, inside it only a trade that also buys
//! back what an earlier one sold is made, and never past the price, past a
//! per-trade cap, or below an asset's floor.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy::network::EthereumWallet;
use alloy::primitives::utils::format_units;
use alloy::primitives::{Address, U256, aliases::U24};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::rpc::types::TransactionReceipt;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, anyhow, bail};
use robinhood::listings::{Feed, MirrorFeeds, Token, Venue};

use crate::bindings::{AggregatorV3Interface, IMirrorFeed, ISwapRouter02, IToken, IUniswapV3Pool};
use crate::health::{FeedHealth, Health, PoolHealth};
use crate::math::{self, Pair, Plan, PoolState};

/// How the pools are kept.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Outside this many basis points from its feed, a pool is taken back.
    pub band_bps: f64,
    /// Inside the band, a trade that buys back what an earlier one sold is
    /// made from this far out.
    pub restore_band_bps: f64,
    /// The most one trade spends, in dollars at the feed's price.
    pub max_trade_usd: f64,
    /// What is never sold, by symbol, in whole tokens.
    pub floors: BTreeMap<String, f64>,
    /// The floor of a stock token not named in `floors`.
    pub default_stock_floor: f64,
    /// A feed older than this is not traded to.
    pub max_reference_age: Duration,
    /// Bitmap words read along a swap's path, at most.
    pub tick_words: usize,
    /// Below this much of the chain's coin, each round says so at warn.
    pub low_gas_wei: U256,
    /// Read and decide, and send nothing.
    pub dry_run: bool,
    /// How often a pool that can't be moved one way is said at warn again.
    pub warn_every: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            band_bps: 30.0,
            restore_band_bps: 5.0,
            max_trade_usd: 250.0,
            floors: [("WETH".to_string(), 0.002), ("USDC".to_string(), 5.0)]
                .into_iter()
                .collect(),
            default_stock_floor: 1.0,
            max_reference_age: Duration::from_secs(4 * 86_400),
            tick_words: 32,
            low_gas_wei: U256::from(200_000_000_000_000u64),
            dry_run: false,
            warn_every: Duration::from_secs(30 * 60),
        }
    }
}

/// A chain's endpoints, the account's signer on each, and which one is in
/// use: a failed round moves on to the next.
pub struct Endpoints {
    urls: Vec<String>,
    wallet: EthereumWallet,
    at: usize,
    provider: DynProvider,
}

impl Endpoints {
    pub fn new(rpc: &str, signer: PrivateKeySigner) -> Result<Self> {
        let urls: Vec<String> = rpc.split_whitespace().map(str::to_string).collect();
        if urls.is_empty() {
            bail!("no endpoint");
        }
        let wallet = EthereumWallet::from(signer);
        let provider = Self::connect(&urls[0], &wallet)?;
        Ok(Self {
            urls,
            wallet,
            at: 0,
            provider,
        })
    }

    fn connect(url: &str, wallet: &EthereumWallet) -> Result<DynProvider> {
        let url = url
            .parse()
            .map_err(|error| anyhow!("bad RPC URL: {error}"))?;
        Ok(ProviderBuilder::new()
            .wallet(wallet.clone())
            .connect_http(url)
            .erased())
    }

    pub fn provider(&self) -> &DynProvider {
        &self.provider
    }

    /// After a failure: the next endpoint, for the next round.
    pub fn rotate(&mut self) {
        if self.urls.len() > 1 {
            self.at = (self.at + 1) % self.urls.len();
            if let Ok(provider) = Self::connect(&self.urls[self.at], &self.wallet) {
                self.provider = provider;
            }
        }
    }
}

/// One mainnet feed and its mirror.
#[derive(Clone, Debug)]
pub struct Mirror {
    pub symbol: String,
    pub source: Address,
    pub mirror: Address,
}

/// One pool kept at its feed's price.
#[derive(Clone, Debug)]
pub struct Kept {
    pub token: Token,
    pub dollar: Token,
    pub feed: Address,
    pub pool: Address,
    pub fee: u32,
}

/// One chain's pools and mirrors.
pub struct ChainMarket {
    pub key: String,
    pub label: String,
    pub endpoints: Endpoints,
    pub router: Address,
    /// The dollar may be minted by anyone (the testnet's USDG).
    pub dollar_mintable: bool,
    pub mirrors: Vec<Mirror>,
    pub pools: Vec<Kept>,
    /// What the account held of each finite asset when it first looked: what
    /// it buys back to.
    pub home: BTreeMap<Address, U256>,
    /// When each pool was last said at warn to be stuck one way.
    warned: BTreeMap<(Address, bool), Instant>,
    /// Each pool's token order, read once.
    orders: BTreeMap<Address, bool>,
    spacings: BTreeMap<Address, i32>,
}

impl ChainMarket {
    /// The pools and mirrors `venue` lists, with every address resolved.
    pub fn new(
        key: String,
        endpoints: Endpoints,
        venue: &Venue,
        mirrors: &MirrorFeeds,
        dollar_mintable: bool,
    ) -> Result<Self> {
        let resolved = venue.resolve(mirrors)?;
        let mut kept_mirrors = Vec::new();
        for listing in venue.listed {
            if let Feed::Mirror { source, .. } = listing.feed {
                let record = &mirrors.feeds[listing.token.symbol];
                kept_mirrors.push(Mirror {
                    symbol: listing.token.symbol.to_string(),
                    source,
                    mirror: record.address,
                });
            }
        }
        Ok(Self {
            key,
            label: venue.label.to_string(),
            endpoints,
            router: venue.uniswap.router,
            dollar_mintable,
            mirrors: kept_mirrors,
            pools: resolved
                .into_iter()
                .map(|resolved| Kept {
                    token: resolved.token,
                    dollar: venue.dollar,
                    feed: resolved.feed,
                    pool: resolved.pool,
                    fee: resolved.fee,
                })
                .collect(),
            home: BTreeMap::new(),
            warned: BTreeMap::new(),
            orders: BTreeMap::new(),
            spacings: BTreeMap::new(),
        })
    }
}

/// What a pool needs this round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// At its price, or within the band with nothing to buy back.
    InBand,
    /// Swap `amount_in` of the pool's token0 (`zero_for_one`) or token1,
    /// with the target as the price limit. `short` says the swap falls
    /// short of the target for want of the asset (not for the cap).
    Swap {
        zero_for_one: bool,
        amount_in: U256,
        short: bool,
    },
    /// The pool is off its price and nothing above the floor is left to
    /// move it.
    Stuck { zero_for_one: bool },
}

/// Everything [`decide`] judges from.
#[derive(Clone, Debug)]
pub struct Situation {
    pub pair: Pair,
    pub pool: PoolState,
    pub target: U256,
    /// The swap to the target, as the ticks read allow.
    pub plan: Plan,
    /// The feed's answer.
    pub answer: U256,
    /// The account's token and dollar, their floors, and what it held at
    /// first.
    pub token_balance: U256,
    pub dollar_balance: U256,
    pub token_floor: U256,
    pub dollar_floor: U256,
    pub token_home: Option<U256>,
    pub dollar_home: Option<U256>,
    /// The dollar can be minted, so it is never short.
    pub dollar_mintable: bool,
}

/// What `situation` calls for under `settings`.
pub fn decide(settings: &Settings, situation: &Situation) -> Decision {
    let s = situation;
    let zero_for_one = s.plan.zero_for_one;
    // Which asset goes in: the dollar or the token.
    let spends_dollar = zero_for_one == s.pair.zero_for_one_spends_dollar();
    let (balance_in, floor_in) = if spends_dollar {
        (s.dollar_balance, s.dollar_floor)
    } else {
        (s.token_balance, s.token_floor)
    };
    // Coming out: what an earlier trade sold, bought back while short of
    // what was held at first.
    let (balance_out, home_out, finite_out) = if spends_dollar {
        (s.token_balance, s.token_home, true)
    } else {
        (s.dollar_balance, s.dollar_home, !s.dollar_mintable)
    };
    let deficit = match home_out {
        Some(home) if finite_out && balance_out < home => home - balance_out,
        _ => U256::ZERO,
    };
    let off = math::deviation_bps(s.pool.sqrt_price, s.target).abs();
    let threshold = if deficit.is_zero() {
        settings.band_bps
    } else {
        settings.restore_band_bps.min(settings.band_bps)
    };
    if off < threshold {
        return Decision::InBand;
    }
    let mut amount = s.plan.amount_in;
    // The cap, in what goes in.
    let cap_dollars = U256::from((settings.max_trade_usd.max(0.0) * 1e6) as u128)
        * U256::from(10u8).pow(U256::from(s.pair.dollar_decimals))
        / U256::from(1_000_000u32);
    let cap = if spends_dollar {
        cap_dollars
    } else {
        s.pair.tokens_for(cap_dollars, s.answer)
    };
    amount = amount.min(cap);
    // Inside the band, only as much as buys back the deficit.
    if off < settings.band_bps && !deficit.is_zero() {
        let deficit_in = if spends_dollar {
            s.pair.dollars_for(deficit, s.answer)
        } else {
            s.pair.tokens_for(deficit, s.answer)
        };
        amount = amount.min(deficit_in.max(U256::from(1u8)));
    }
    let available = if spends_dollar && s.dollar_mintable {
        U256::MAX
    } else {
        balance_in.saturating_sub(floor_in)
    };
    if available.is_zero() {
        // Inside the band nothing has to move: only a buy-back is missed.
        if off < settings.band_bps {
            return Decision::InBand;
        }
        return Decision::Stuck { zero_for_one };
    }
    let short = available < amount;
    amount = amount.min(available);
    if amount.is_zero() {
        return Decision::InBand;
    }
    Decision::Swap {
        zero_for_one,
        amount_in: amount,
        short,
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

fn units(amount: U256, decimals: u8) -> String {
    format_units(amount, decimals).unwrap_or_else(|_| amount.to_string())
}

fn whole(amount: f64, decimals: u8) -> U256 {
    // Floors are small and written by hand: micro-units are exact enough.
    U256::from((amount.max(0.0) * 1e6) as u128) * U256::from(10u8).pow(U256::from(decimals))
        / U256::from(1_000_000u32)
}

/// What one sent transaction cost.
fn gas_line(receipt: &TransactionReceipt) -> (u64, String) {
    let cost = U256::from(receipt.gas_used) * U256::from(receipt.effective_gas_price);
    (receipt.gas_used, units(cost, 18))
}

/// The service: every chain it keeps, the mainnet the feeds come from, and
/// what it reports.
pub struct Market {
    pub account: Address,
    pub source: DynProvider,
    pub chains: Vec<ChainMarket>,
    pub settings: Settings,
    pub health: Arc<Mutex<Health>>,
}

impl Market {
    /// One round over every chain. A chain that fails is said, its
    /// endpoint moved on, and the others carry on.
    pub async fn round(&mut self) {
        let started = now();
        let mut failures = 0usize;
        for index in 0..self.chains.len() {
            if let Err(error) = self.keep_chain(index).await {
                failures += 1;
                let chain = &mut self.chains[index];
                tracing::warn!(chain = %chain.key, error = %format!("{error:#}"), "the round on this chain stopped; trying its next endpoint next round");
                chain.endpoints.rotate();
                if let Ok(mut health) = self.health.lock() {
                    health.chain(&chain.key).error = Some(format!("{error:#}"));
                }
            }
        }
        if let Ok(mut health) = self.health.lock() {
            health.last_round = Some(started);
            health.rounds += 1;
            if failures == 0 {
                health.last_clean_round = Some(started);
            }
        }
    }

    async fn keep_chain(&mut self, index: usize) -> Result<()> {
        let key = self.chains[index].key.clone();
        let gas = self.chains[index]
            .endpoints
            .provider()
            .get_balance(self.account)
            .await
            .context("reading the account's gas")?;
        if gas < self.settings.low_gas_wei {
            tracing::warn!(chain = %key, gas = %units(gas, 18), "the account is low on gas: top it up from the funder");
        }
        if let Ok(mut health) = self.health.lock() {
            let chain = health.chain(&key);
            chain.label = self.chains[index].label.clone();
            chain.gas = units(gas, 18);
            chain.error = None;
        }
        for m in 0..self.chains[index].mirrors.len() {
            self.mirror(index, m).await?;
        }
        for p in 0..self.chains[index].pools.len() {
            self.keep_pool(index, p).await?;
        }
        Ok(())
    }

    /// Post the mainnet feed's latest round to its mirror if it is new.
    async fn mirror(&mut self, index: usize, m: usize) -> Result<()> {
        let chain = &self.chains[index];
        let mirror = chain.mirrors[m].clone();
        let source = AggregatorV3Interface::new(mirror.source, &self.source)
            .latestRoundData()
            .call()
            .await
            .with_context(|| format!("reading {}'s mainnet feed", mirror.symbol))?;
        let provider = chain.endpoints.provider().clone();
        let copy = IMirrorFeed::new(mirror.mirror, &provider);
        let latest = AggregatorV3Interface::new(mirror.mirror, &provider)
            .latestRoundData()
            .call()
            .await
            .with_context(|| format!("reading {}'s mirror feed", mirror.symbol))?;
        let source_updated = u64::try_from(source.updatedAt).unwrap_or(u64::MAX);
        let mut health = FeedHealth {
            symbol: mirror.symbol.clone(),
            source_round: source.roundId.to_string(),
            mirror_round: latest.roundId.to_string(),
            source_updated_at: source_updated,
            mirror_updated_at: u64::try_from(latest.updatedAt).unwrap_or(u64::MAX),
            last_post: None,
        };
        let newer = source.roundId > latest.roundId && source.updatedAt >= latest.updatedAt;
        if source.roundId < latest.roundId {
            tracing::warn!(chain = %chain.key, token = %mirror.symbol, source = %source.roundId, mirror = %latest.roundId, "the mirror is ahead of its mainnet feed: nothing posted");
        }
        if newer {
            if self.settings.dry_run {
                tracing::info!(chain = %chain.key, token = %mirror.symbol, round = %source.roundId, answer = %source.answer, "would post the mainnet round (dry run)");
            } else {
                let receipt = copy
                    .update(source.roundId, source.answer, source.updatedAt)
                    .send()
                    .await
                    .with_context(|| format!("posting {}'s round", mirror.symbol))?
                    .get_receipt()
                    .await
                    .with_context(|| format!("waiting for {}'s post", mirror.symbol))?;
                let (gas, cost) = gas_line(&receipt);
                if !receipt.status() {
                    bail!(
                        "{}'s post {} reverted",
                        mirror.symbol,
                        receipt.transaction_hash
                    );
                }
                tracing::info!(chain = %chain.key, token = %mirror.symbol, round = %source.roundId, answer = %source.answer, updated_at = source_updated, transaction = %receipt.transaction_hash, gas, cost_eth = %cost, "posted the mainnet round to the mirror");
                health.mirror_round = source.roundId.to_string();
                health.mirror_updated_at = source_updated;
                health.last_post = Some(now());
            }
        }
        if let Ok(mut shared) = self.health.lock() {
            let feeds = &mut shared.chain(&chain.key).feeds;
            let last_post = feeds
                .iter()
                .find(|feed| feed.symbol == health.symbol)
                .and_then(|feed| feed.last_post);
            health.last_post = health.last_post.or(last_post);
            feeds.retain(|feed| feed.symbol != health.symbol);
            feeds.push(health);
        }
        Ok(())
    }

    fn floor_of(&self, token: &Token) -> U256 {
        let floor = self
            .settings
            .floors
            .get(token.symbol)
            .copied()
            .unwrap_or(self.settings.default_stock_floor);
        whole(floor, token.decimals)
    }

    async fn keep_pool(&mut self, index: usize, p: usize) -> Result<()> {
        let account = self.account;
        let settings = self.settings.clone();
        let kept = self.chains[index].pools[p].clone();
        let provider = self.chains[index].endpoints.provider().clone();
        let symbol = kept.token.symbol;
        let key = self.chains[index].key.clone();

        let round = AggregatorV3Interface::new(kept.feed, &provider)
            .latestRoundData()
            .call()
            .await
            .with_context(|| format!("reading {symbol}'s feed"))?;
        if round.answer.is_negative() || round.answer.is_zero() {
            bail!("{symbol}'s feed answers {}", round.answer);
        }
        let answer = round.answer.into_raw();
        let updated_at = u64::try_from(round.updatedAt).unwrap_or(u64::MAX);
        let pool = IUniswapV3Pool::new(kept.pool, &provider);
        let token_is_token0 = match self.chains[index].orders.get(&kept.pool) {
            Some(order) => *order,
            None => {
                let token0 = pool
                    .token0()
                    .call()
                    .await
                    .with_context(|| format!("reading {symbol}'s pool"))?;
                let order = token0 == kept.token.address;
                self.chains[index].orders.insert(kept.pool, order);
                order
            }
        };
        let pair = Pair {
            token_is_token0,
            token_decimals: kept.token.decimals,
            dollar_decimals: kept.dollar.decimals,
            feed_decimals: robinhood::listings::FEED_DECIMALS,
        };
        let slot0 = pool.slot0().call().await?;
        let liquidity = pool.liquidity().call().await?;
        let state = PoolState {
            sqrt_price: U256::from(slot0.sqrtPriceX96),
            tick: slot0.tick.as_i32(),
            liquidity,
            fee: kept.fee,
        };
        let target = pair
            .sqrt_price_at(answer)
            .ok_or_else(|| anyhow!("{symbol}'s feed price is outside any pool's range"))?;
        let premium = math::token_premium_bps(&pair, state.sqrt_price, target);
        let pool_price = price_of(&pair, state.sqrt_price);
        let reference = math::to_f64(answer) / 1e8;
        let mut health = PoolHealth {
            symbol: symbol.to_string(),
            pool: kept.pool.to_string(),
            reference,
            pool_price,
            premium_bps: premium,
            feed_updated_at: updated_at,
            last_trade: None,
            stuck: None,
        };

        let age = now().saturating_sub(updated_at);
        if age > settings.max_reference_age.as_secs() {
            tracing::warn!(chain = %key, token = symbol, age_hours = age / 3_600, "the feed is older than the market trades to: the pool is left as it is");
            self.record_pool(&key, health);
            return Ok(());
        }

        let token = IToken::new(kept.token.address, &provider);
        let dollar = IToken::new(kept.dollar.address, &provider);
        let token_balance = token.balanceOf(account).call().await?;
        let dollar_balance = dollar.balanceOf(account).call().await?;
        let mintable = self.chains[index].dollar_mintable;
        let home = &mut self.chains[index].home;
        let token_home = *home.entry(kept.token.address).or_insert(token_balance);
        let dollar_home = if mintable {
            None
        } else {
            Some(*home.entry(kept.dollar.address).or_insert(dollar_balance))
        };

        let (crossed, read_to) = if target == state.sqrt_price {
            (Vec::new(), None)
        } else {
            self.ticks_crossed(index, &pool, &state, target).await?
        };
        let Some(plan) = math::plan_to(&state, target, &crossed, read_to) else {
            self.record_pool(&key, health);
            return Ok(());
        };
        let situation = Situation {
            pair,
            pool: state,
            target,
            plan,
            answer,
            token_balance,
            dollar_balance,
            token_floor: self.floor_of(&kept.token),
            dollar_floor: if mintable {
                U256::ZERO
            } else {
                self.floor_of(&kept.dollar)
            },
            token_home: Some(token_home),
            dollar_home,
            dollar_mintable: mintable,
        };
        let decision = decide(&settings, &situation);
        let direction = |zero_for_one: bool| {
            if zero_for_one == pair.zero_for_one_spends_dollar() {
                "buy"
            } else {
                "sell"
            }
        };
        match decision {
            Decision::InBand => {}
            Decision::Stuck { zero_for_one } => {
                let (asset, held, floor) = if zero_for_one == pair.zero_for_one_spends_dollar() {
                    (kept.dollar, dollar_balance, situation.dollar_floor)
                } else {
                    (kept.token, token_balance, situation.token_floor)
                };
                let way = if direction(zero_for_one) == "buy" {
                    "up"
                } else {
                    "down"
                };
                health.stuck = Some(format!(
                    "can't move it {way}: no {} above the floor",
                    asset.symbol
                ));
                self.warn_stuck(index, kept.pool, zero_for_one, || {
                    tracing::warn!(chain = %key, token = symbol, premium_bps = format!("{premium:.1}"), way, asset = asset.symbol, held = %units(held, asset.decimals), floor = %units(floor, asset.decimals), "can't move the pool: nothing above the floor to sell into it");
                });
            }
            Decision::Swap {
                zero_for_one,
                amount_in,
                short,
            } => {
                let spends_dollar = zero_for_one == pair.zero_for_one_spends_dollar();
                let (asset_in, asset_out) = if spends_dollar {
                    (kept.dollar, kept.token)
                } else {
                    (kept.token, kept.dollar)
                };
                if short {
                    let way = if spends_dollar { "up" } else { "down" };
                    health.stuck = Some(format!(
                        "moving it {way} only part of the way: short of {}",
                        asset_in.symbol
                    ));
                    self.warn_stuck(index, kept.pool, zero_for_one, || {
                        tracing::warn!(chain = %key, token = symbol, premium_bps = format!("{premium:.1}"), way, asset = asset_in.symbol, "can't move the pool all the way: not enough above the floor");
                    });
                }
                if settings.dry_run {
                    tracing::info!(chain = %key, token = symbol, side = direction(zero_for_one), premium_bps = format!("{premium:.1}"), amount_in = %units(amount_in, asset_in.decimals), asset_in = asset_in.symbol, "would swap (dry run)");
                } else {
                    let router = self.chains[index].router;
                    if spends_dollar && mintable && dollar_balance < amount_in {
                        let receipt = dollar
                            .mint(account, amount_in - dollar_balance)
                            .send()
                            .await?
                            .get_receipt()
                            .await?;
                        let (gas, cost) = gas_line(&receipt);
                        if !receipt.status() {
                            bail!("minting {} reverted", kept.dollar.symbol);
                        }
                        tracing::info!(chain = %key, minted = %units(amount_in - dollar_balance, asset_in.decimals), asset = asset_in.symbol, transaction = %receipt.transaction_hash, gas, cost_eth = %cost, "minted the dollar");
                    }
                    let spent = IToken::new(asset_in.address, &provider);
                    if spent.allowance(account, router).call().await? < amount_in {
                        let receipt = spent
                            .approve(router, U256::MAX)
                            .send()
                            .await?
                            .get_receipt()
                            .await?;
                        let (gas, cost) = gas_line(&receipt);
                        tracing::info!(chain = %key, asset = asset_in.symbol, transaction = %receipt.transaction_hash, gas, cost_eth = %cost, "approved the router");
                    }
                    let out_before = IToken::new(asset_out.address, &provider)
                        .balanceOf(account)
                        .call()
                        .await?;
                    let in_before = spent.balanceOf(account).call().await?;
                    let receipt = ISwapRouter02::new(router, &provider)
                        .exactInputSingle(ISwapRouter02::ExactInputSingleParams {
                            tokenIn: asset_in.address,
                            tokenOut: asset_out.address,
                            fee: U24::from(kept.fee),
                            recipient: account,
                            amountIn: amount_in,
                            // The price limit is the guard: the swap stops at
                            // the feed's price, whatever it would fetch.
                            amountOutMinimum: U256::ZERO,
                            sqrtPriceLimitX96: alloy::primitives::aliases::U160::from(target),
                        })
                        .send()
                        .await
                        .with_context(|| format!("swapping in {symbol}'s pool"))?
                        .get_receipt()
                        .await?;
                    let (gas, cost) = gas_line(&receipt);
                    if !receipt.status() {
                        bail!(
                            "the swap in {symbol}'s pool {} reverted",
                            receipt.transaction_hash
                        );
                    }
                    let out_after = IToken::new(asset_out.address, &provider)
                        .balanceOf(account)
                        .call()
                        .await?;
                    let in_after = spent.balanceOf(account).call().await?;
                    let paid = in_before.saturating_sub(in_after);
                    let got = out_after.saturating_sub(out_before);
                    let after = IUniswapV3Pool::new(kept.pool, &provider)
                        .slot0()
                        .call()
                        .await?;
                    let premium_after =
                        math::token_premium_bps(&pair, U256::from(after.sqrtPriceX96), target);
                    tracing::info!(
                        chain = %key,
                        token = symbol,
                        side = direction(zero_for_one),
                        paid = %units(paid, asset_in.decimals),
                        asset_in = asset_in.symbol,
                        got = %units(got, asset_out.decimals),
                        asset_out = asset_out.symbol,
                        premium_bps_before = format!("{premium:.1}"),
                        premium_bps_after = format!("{premium_after:.1}"),
                        reference,
                        transaction = %receipt.transaction_hash,
                        gas,
                        cost_eth = %cost,
                        "swapped the pool towards its feed"
                    );
                    health.last_trade = Some(now());
                    health.premium_bps = premium_after;
                    health.pool_price = price_of(&pair, U256::from(after.sqrtPriceX96));
                }
            }
        }
        self.record_pool(&key, health);
        Ok(())
    }

    fn warn_stuck(&mut self, index: usize, pool: Address, zero_for_one: bool, say: impl FnOnce()) {
        let warned = &mut self.chains[index].warned;
        let due = warned
            .get(&(pool, zero_for_one))
            .is_none_or(|at| at.elapsed() >= self.settings.warn_every);
        if due {
            say();
            warned.insert((pool, zero_for_one), Instant::now());
        }
    }

    fn record_pool(&self, key: &str, mut health: PoolHealth) {
        if let Ok(mut shared) = self.health.lock() {
            let pools = &mut shared.chain(key).pools;
            let last_trade = pools
                .iter()
                .find(|pool| pool.symbol == health.symbol)
                .and_then(|pool| pool.last_trade);
            health.last_trade = health.last_trade.or(last_trade);
            pools.retain(|pool| pool.symbol != health.symbol);
            pools.push(health);
        }
    }

    /// The initialised ticks between the pool's price and the target, in
    /// the order the price meets them, and how far the words read reach
    /// when they stop short of the target.
    async fn ticks_crossed(
        &mut self,
        index: usize,
        pool: &IUniswapV3Pool::IUniswapV3PoolInstance<&DynProvider>,
        state: &PoolState,
        target: U256,
    ) -> Result<(Vec<(i32, i128)>, Option<i32>)> {
        let spacing = match self.chains[index].spacings.get(pool.address()) {
            Some(spacing) => *spacing,
            None => {
                let spacing = pool.tickSpacing().call().await?.as_i32();
                if spacing <= 0 {
                    bail!("the pool's tick spacing is {spacing}");
                }
                self.chains[index].spacings.insert(*pool.address(), spacing);
                spacing
            }
        };
        let down = target < state.sqrt_price;
        let target_tick = math::tick_at_sqrt_ratio(target);
        let first = math::word_of(state.tick, spacing);
        let last = math::word_of(target_tick, spacing);
        let mut ticks = Vec::new();
        let mut read_to = None;
        let mut word = first;
        let mut read = 0usize;
        loop {
            if read == self.settings.tick_words {
                // The walk can go as far as the edge of the last word read.
                let edge = if down {
                    (i32::from(word) + 1) << 8
                } else {
                    i32::from(word) << 8
                };
                read_to = Some(edge * spacing);
                break;
            }
            let bits = pool.tickBitmap(word).call().await?;
            read += 1;
            let mut found = math::ticks_in_word(word, bits, spacing);
            if down {
                found.retain(|tick| *tick <= state.tick && *tick > target_tick);
                found.reverse();
            } else {
                found.retain(|tick| *tick > state.tick && *tick <= target_tick);
            }
            for tick in found {
                let info = pool
                    .ticks(
                        alloy::primitives::aliases::I24::try_from(tick)
                            .map_err(|_| anyhow!("tick {tick}"))?,
                    )
                    .call()
                    .await?;
                ticks.push((tick, info.liquidityNet));
            }
            if word == last {
                break;
            }
            word = if down { word - 1 } else { word + 1 };
        }
        Ok((ticks, read_to))
    }
}

/// The token's price in dollars, as the pool has it.
fn price_of(pair: &Pair, sqrt_price: U256) -> f64 {
    let raw = (math::to_f64(sqrt_price) / 2f64.powi(96)).powi(2);
    let per_unit = if pair.token_is_token0 { raw } else { 1.0 / raw };
    per_unit * 10f64.powi(i32::from(pair.token_decimals) - i32::from(pair.dollar_decimals))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(x: u128) -> U256 {
        U256::from(x)
    }

    const E18: u128 = 1_000_000_000_000_000_000;

    /// A stock at `pool_usd` in a pool of 5 tokens, the feed at $400; the
    /// account holds `tokens` of it and `dollars`.
    fn situation(token_is_token0: bool, pool_usd: u128, tokens: u128, dollars: u128) -> Situation {
        let pair = Pair {
            token_is_token0,
            token_decimals: 18,
            dollar_decimals: 6,
            feed_decimals: 8,
        };
        let answer = d(400_00000000);
        let target = pair.sqrt_price_at(answer).unwrap();
        let sqrt_price = pair.sqrt_price_at(d(pool_usd * 100_000_000)).unwrap();
        let pool = PoolState {
            sqrt_price,
            tick: math::tick_at_sqrt_ratio(sqrt_price),
            liquidity: 100_000_000_000_000,
            fee: 500,
        };
        let plan = math::plan_to(&pool, target, &[], None).unwrap_or(Plan {
            zero_for_one: true,
            amount_in: U256::ZERO,
            amount_out: U256::ZERO,
            reaches: true,
        });
        Situation {
            pair,
            pool,
            target,
            plan,
            answer,
            token_balance: d(tokens),
            dollar_balance: d(dollars),
            token_floor: d(E18),
            dollar_floor: U256::ZERO,
            token_home: Some(d(tokens)),
            dollar_home: None,
            dollar_mintable: true,
        }
    }

    fn spends_dollar(s: &Situation, decision: &Decision) -> bool {
        match decision {
            Decision::Swap { zero_for_one, .. } | Decision::Stuck { zero_for_one } => {
                *zero_for_one == s.pair.zero_for_one_spends_dollar()
            }
            Decision::InBand => panic!("no trade"),
        }
    }

    #[test]
    fn a_pool_within_the_band_is_left_alone() {
        let settings = Settings::default();
        for order in [true, false] {
            let s = situation(order, 400, 10 * E18, 0);
            assert_eq!(decide(&settings, &s), Decision::InBand);
        }
    }

    #[test]
    fn a_cheap_pool_is_bought_with_minted_dollars_and_a_dear_one_sold_into_in_either_order() {
        let settings = Settings::default();
        for order in [true, false] {
            let cheap = situation(order, 380, 10 * E18, 0);
            let decision = decide(&settings, &cheap);
            assert!(
                matches!(decision, Decision::Swap { short: false, .. }),
                "{decision:?}"
            );
            assert!(spends_dollar(&cheap, &decision), "a cheap token is bought");
            let dear = situation(order, 420, 10 * E18, 0);
            let decision = decide(&settings, &dear);
            assert!(matches!(decision, Decision::Swap { .. }), "{decision:?}");
            assert!(!spends_dollar(&dear, &decision), "a dear token is sold");
        }
    }

    #[test]
    fn a_trade_is_capped_at_its_dollars_at_the_feed() {
        let settings = Settings {
            max_trade_usd: 10.0,
            ..Settings::default()
        };
        for order in [true, false] {
            let cheap = situation(order, 300, 10 * E18, 0);
            let Decision::Swap { amount_in, .. } = decide(&settings, &cheap) else {
                panic!()
            };
            assert_eq!(amount_in, d(10_000_000), "ten dollars in");
            let dear = situation(order, 500, 10 * E18, 0);
            let Decision::Swap { amount_in, .. } = decide(&settings, &dear) else {
                panic!()
            };
            assert_eq!(
                amount_in,
                d(25_000_000_000_000_000),
                "ten dollars of a $400 token"
            );
        }
    }

    #[test]
    fn nothing_below_the_floor_is_sold_and_a_pool_it_cannot_move_is_stuck() {
        let settings = Settings::default();
        for order in [true, false] {
            // Exactly the floor held: nothing to sell.
            let dear = situation(order, 420, E18, 0);
            let decision = decide(&settings, &dear);
            assert!(matches!(decision, Decision::Stuck { .. }), "{decision:?}");
            assert!(!spends_dollar(&dear, &decision));
            // A little above it: that little, and short of the target.
            let dear = situation(order, 420, E18 + 1_000, 0);
            assert_eq!(
                decide(&settings, &dear),
                Decision::Swap {
                    zero_for_one: dear.plan.zero_for_one,
                    amount_in: d(1_000),
                    short: true
                }
            );
        }
    }

    #[test]
    fn a_finite_dollar_is_never_spent_below_its_floor() {
        let settings = Settings::default();
        let mut cheap = situation(true, 380, 10 * E18, 5_000_000);
        cheap.dollar_mintable = false;
        cheap.dollar_floor = d(5_000_000);
        cheap.dollar_home = Some(d(5_000_000));
        assert!(matches!(decide(&settings, &cheap), Decision::Stuck { .. }));
    }

    #[test]
    fn inside_the_band_only_what_was_sold_is_bought_back_and_only_towards_the_price() {
        let settings = Settings::default();
        for order in [true, false] {
            // 0.2% cheap: inside the 0.3% band. Holding 2 tokens after
            // starting with 2.01: 0.01 is bought back.
            let mut s = situation(order, 400, 2 * E18, 0);
            let target_usd = 399_20000000u128;
            s.pool.sqrt_price = s.pair.sqrt_price_at(d(target_usd)).unwrap();
            s.plan = math::plan_to(&s.pool, s.target, &[], None).unwrap();
            assert_eq!(
                decide(&settings, &s),
                Decision::InBand,
                "nothing sold, nothing to buy back"
            );
            s.token_home = Some(d(2 * E18 + E18 / 100));
            let decision = decide(&settings, &s);
            let Decision::Swap { amount_in, .. } = decision else {
                panic!("{decision:?}")
            };
            assert!(spends_dollar(&s, &decision));
            assert_eq!(
                amount_in, s.plan.amount_in,
                "a shallow pool: no further than the price, short of the deficit"
            );
            assert!(amount_in < d(4_000_000));
            // A deep pool: the deficit, not the price, is the limit.
            s.pool.liquidity *= 100;
            s.plan = math::plan_to(&s.pool, s.target, &[], None).unwrap();
            let decision = decide(&settings, &s);
            let Decision::Swap { amount_in, .. } = decision else {
                panic!("{decision:?}")
            };
            assert_eq!(amount_in, d(4_000_000), "0.01 of a $400 token in dollars");
            // The same deficit when the pool is dear: buying would move it
            // away from the price, so nothing.
            let mut dear = s.clone();
            dear.pool.sqrt_price = dear.pair.sqrt_price_at(d(400_80000000)).unwrap();
            dear.plan = math::plan_to(&dear.pool, dear.target, &[], None).unwrap();
            assert_eq!(decide(&settings, &dear), Decision::InBand);
        }
    }

    #[test]
    fn base_s_usdc_bought_back_inside_the_band_when_weth_was_bought() {
        // WETH at $2,400 in the pool, the feed at $2,397.6 (0.1% dear):
        // having spent USDC buying WETH earlier, the account sells WETH back.
        let pair = Pair {
            token_is_token0: false,
            token_decimals: 18,
            dollar_decimals: 6,
            feed_decimals: 8,
        };
        let answer = d(239_760_000_000);
        let target = pair.sqrt_price_at(answer).unwrap();
        let sqrt_price = pair.sqrt_price_at(d(240_000_000_000)).unwrap();
        let pool = PoolState {
            sqrt_price,
            tick: math::tick_at_sqrt_ratio(sqrt_price),
            liquidity: 448_000_000_000,
            fee: 500,
        };
        let plan = math::plan_to(&pool, target, &[], None).unwrap();
        let s = Situation {
            pair,
            pool,
            target,
            plan,
            answer,
            token_balance: d(E18 / 10),
            dollar_balance: d(40_000_000),
            token_floor: d(2 * E18 / 1_000),
            dollar_floor: d(5_000_000),
            token_home: Some(d(E18 / 20)),
            dollar_home: Some(d(50_000_000)),
            dollar_mintable: false,
        };
        let decision = decide(&Settings::default(), &s);
        assert!(matches!(decision, Decision::Swap { .. }), "{decision:?}");
        assert!(!spends_dollar(&s, &decision), "WETH is sold for USDC");
    }
}
