//! Robinhood Chain's two drivers' executor halves, as this executor runs
//! them.
//!
//! - **`robinhood`**: stock tokens in the station's Safe, traded on Uniswap
//!   by this vault's key as a member of the Safe's Roles v2 role. A swap is
//!   checked against the token's Chainlink price and the station's band here
//!   (`robinhood::trader`) before the key signs, and again by the role
//!   on-chain.
//! - **`lighter`**: the station's Lighter account, traded with a Lighter key
//!   made here at every start. The vault's key names the new key's public
//!   half on the board and has the station register it, so a restarted (or
//!   replaced) executor's old key stops working. Only orders, cancels and
//!   modifications are signed (`lighter::trader`). The same half posts the
//!   attested read of the station's accounts on the board, which values the
//!   station and proves its staged closes.

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, B256, U256};
use anyhow::{Context, Result, anyhow, bail};
use chain::{Client, KeyClient};
use hyperliquid::agent::Outcome;
use tokio::sync::Mutex;

/// The names `shonin.toml` writes for them.
pub const ROBINHOOD: &str = "robinhood";
pub const LIGHTER: &str = "lighter";

/// How often the attested read of a station's Lighter accounts is posted:
/// the driver stages a claim's close only on a post at most three minutes
/// old, so a post every minute leaves room for two to fail.
pub const REPORT_EVERY: Duration = Duration::from_secs(60);
/// How often the post's loop looks at whether a staged close is waiting for
/// a post read after its settle delay, which it then makes at once.
const STAGE_LOOK_EVERY: Duration = Duration::from_secs(5);
/// How long after a post that failed the next is tried.
const RETRY_AFTER: Duration = Duration::from_secs(15);
/// The most one post may take, reads and send: past it, the round is given
/// up and the next one made on time.
const POST_TIMEOUT: Duration = Duration::from_secs(90);
/// The most time between the start of a post's reads of the exchange's API
/// and its read of the chain (the pending balance and the station's
/// counters): the post says both were true together, so it is not made when
/// they are further apart, nor when the chain's newest block is older than
/// the API reads by more than this.
pub const POST_READ_SPAN: Duration = Duration::from_secs(30);

alloy::sol! {
    interface IStationRh {
        function pinnedDriver(bytes32 driver) external view returns (address);
        function approveKey(bytes32 driver, address key) external;
        function approvedKey(bytes32 driver) external view returns (address);
        function arrivals() external view returns (uint256);
        function venueFlows(bytes32 driver) external view returns (uint256 sent, uint64 depositedAt);
    }
    interface ILighterDriver {
        struct Market { uint16 index; uint8 sizeDecimals; uint8 priceDecimals; }
        function exchange() external view returns (address);
        function board() external view returns (address);
        function markets() external view returns (Market[] memory);
    }
    interface IExchange {
        function addressToAccountIndex(address owner) external view returns (uint48);
        function getPendingBalance(address owner, uint16 assetIndex) external view returns (uint128);
    }
}

/// A driver's name as a station keys it: the name, zero-padded.
fn name(driver: &str) -> B256 {
    let mut bytes = [0u8; 32];
    bytes[..driver.len()].copy_from_slice(driver.as_bytes());
    B256::from(bytes)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0)
}

/// Whether a send's failure may still land, as the agent is told.
fn outcome_of(error: anyhow::Error) -> Outcome<Vec<u8>> {
    if chain::may_still_land(&error) {
        Outcome::Unknown(format!("{error:#}"))
    } else {
        Outcome::Refused(format!("{error:#}"))
    }
}

// Stock tokens ------------------------------------------------------------------

/// One vault's Safe and the role its key trades through.
pub struct StockHalf {
    trader: robinhood::Trader,
    chain: Client,
    key: KeyClient,
}

impl StockHalf {
    pub async fn connect(chain: &Client, key: KeyClient, station: Address) -> Result<Arc<Self>> {
        let driver = chain
            .read(
                station,
                IStationRh::pinnedDriverCall {
                    driver: name(ROBINHOOD),
                },
            )
            .await
            .context("reading the station's pinned Robinhood driver")?;
        let trader = robinhood::Trader::load(chain.provider(), driver, station).await?;
        Ok(Arc::new(Self {
            trader,
            chain: chain.clone(),
            key,
        }))
    }

    pub async fn submit(&self, payload: &[u8]) -> Outcome<Vec<u8>> {
        let action: robinhood::payload::Action = match strategy_sdk::from_payload(payload) {
            Ok(action) => action,
            Err(error) => {
                return Outcome::Refused(format!("that is not a Robinhood action: {error}"));
            }
        };
        let token = match &action {
            robinhood::payload::Action::Buy { token, .. }
            | robinhood::payload::Action::Sell { token, .. } => token.clone(),
        };
        let bounds = match self.trader.bounds(self.chain.provider(), &token).await {
            Ok(bounds) => bounds,
            Err(error) => return Outcome::Refused(format!("{error:#}")),
        };
        let calls = match self.trader.prepare(&action, &bounds) {
            Ok(calls) => calls,
            Err(error) => return Outcome::Refused(format!("{error:#}")),
        };
        let mut last = None;
        for call in calls {
            match self.key.send(call.to, call.call).await {
                Ok(receipt) => last = Some(receipt.transaction_hash),
                Err(error) => return outcome_of(error),
            }
        }
        let (spent, received) = match &action {
            robinhood::payload::Action::Buy {
                amount, min_tokens, ..
            } => (*amount, *min_tokens),
            robinhood::payload::Action::Sell {
                tokens, min_amount, ..
            } => (*tokens, *min_amount),
        };
        Outcome::Accepted(strategy_sdk::to_payload(
            &robinhood::payload::Receipt::Swapped {
                token,
                spent,
                // At least this much: the swap reverts below it.
                received,
                transaction: last.map(|hash| hash.to_string()).unwrap_or_default(),
            },
        ))
    }

    /// What the agent asked: the Safe's holdings priced at their feeds, or
    /// every listed token's feed and pool prices.
    pub async fn state(&self, query: &[u8]) -> Result<Vec<u8>> {
        use robinhood::contracts::{IAggregatorV3, IRobinhoodDriver, IStockToken};
        use robinhood::payload::{AccountState, Decimal, Holding, Query, TokenPrice};
        let query = if query.is_empty() {
            Query::Holdings
        } else {
            strategy_sdk::from_payload(query)
                .map_err(|error| anyhow!("not a Robinhood query: {error}"))?
        };
        let provider = self.chain.provider();
        let units = |value: U256, decimals: u8| -> Decimal {
            let raw: Decimal = value.to_string().parse().unwrap_or_default();
            raw / Decimal::from(10u64).powi(i32::from(decimals))
        };
        let whole = U256::from(10u64).pow(U256::from(18u8));
        match query {
            Query::Holdings => {
                let dollars = IStockToken::new(self.trader.dollar, provider)
                    .balanceOf(self.trader.safe)
                    .call()
                    .await?;
                let mut holdings = Vec::new();
                for listed in self.trader.listed.values() {
                    let token = IStockToken::new(listed.token, provider);
                    let balance = token.balanceOf(self.trader.safe).call().await?;
                    if balance.is_zero() {
                        continue;
                    }
                    let round = IAggregatorV3::new(listed.feed, provider)
                        .latestRoundData()
                        .call()
                        .await?;
                    let multiplier = token.uiMultiplier().call().await.unwrap_or(whole);
                    holdings.push(Holding {
                        token: listed.symbol.clone(),
                        tokens: units(balance, listed.token_decimals),
                        feed_price: units(round.answer.into_raw(), listed.feed_decimals),
                        feed_updated_at: u64::try_from(round.updatedAt).unwrap_or(0),
                        multiplier: units(multiplier, 18),
                    });
                }
                Ok(strategy_sdk::to_payload(&AccountState {
                    safe: self.trader.safe.to_string(),
                    dollars: units(dollars, self.trader.dollar_decimals),
                    holdings,
                }))
            }
            Query::Prices => {
                let driver = IRobinhoodDriver::new(self.trader.driver, provider);
                let mut prices = Vec::new();
                for listed in self.trader.listed.values() {
                    let token = IStockToken::new(listed.token, provider);
                    let round = IAggregatorV3::new(listed.feed, provider)
                        .latestRoundData()
                        .call()
                        .await?;
                    let one = U256::from(10u64).pow(U256::from(listed.token_decimals));
                    let market = driver
                        .marketQuote(listed.pool, listed.token, one)
                        .call()
                        .await
                        .unwrap_or_default();
                    prices.push(TokenPrice {
                        token: listed.symbol.clone(),
                        feed_price: units(round.answer.into_raw(), listed.feed_decimals),
                        feed_updated_at: u64::try_from(round.updatedAt).unwrap_or(0),
                        market_price: units(market, self.trader.dollar_decimals),
                        multiplier: units(token.uiMultiplier().call().await.unwrap_or(whole), 18),
                        paused: token.oraclePaused().call().await.unwrap_or(false),
                    });
                }
                Ok(strategy_sdk::to_payload(&prices))
            }
        }
    }
}

// Lighter -------------------------------------------------------------------------

/// One vault's Lighter account, and the Lighter key made for it at this start.
pub struct LighterHalf {
    chain: Client,
    key: KeyClient,
    station: Address,
    exchange: Address,
    board: Address,
    /// The station's pinned Lighter driver, whose markets are the only ones
    /// the key signs in.
    driver: Address,
    api: lighter::Api,
    chain_id: u32,
    lighter_key: lighter::Key,
    /// Built once the station has an account and has registered this key.
    trader: Mutex<Option<(u64, lighter::Trader)>>,
}

impl LighterHalf {
    /// Make a Lighter key, name it on the board as this vault's key, and
    /// have the station register it if the account already exists.
    pub async fn connect(chain: &Client, key: KeyClient, station: Address) -> Result<Arc<Self>> {
        let (api, chain_id) = match chain.chain_id {
            robinhood::listings::MAINNET => (
                lighter::Api::new(lighter::api::MAINNET),
                lighter::api::MAINNET_CHAIN_ID,
            ),
            other => bail!(
                "Lighter has no exchange on chain {other} (its testnet settles on Ethereum Sepolia)"
            ),
        };
        let driver = chain
            .read(
                station,
                IStationRh::pinnedDriverCall {
                    driver: name(LIGHTER),
                },
            )
            .await
            .context("reading the station's pinned Lighter driver")?;
        let exchange = chain.read(driver, ILighterDriver::exchangeCall {}).await?;
        let board = chain.read(driver, ILighterDriver::boardCall {}).await?;
        let lighter_key = lighter::Key::generate();
        key.send(
            board,
            lighter::report::ILighterBoard::setPubKeyCall {
                pubKey: lighter_key.public().to_vec().into(),
            },
        )
        .await
        .context("naming this start's Lighter key on the board")?;
        let half = Arc::new(Self {
            chain: chain.clone(),
            key,
            station,
            exchange,
            board,
            driver,
            api,
            chain_id,
            lighter_key,
            trader: Mutex::new(None),
        });
        // Best effort now; tried again before the first order.
        if let Err(error) = half.trader().await {
            tracing::info!(%station, error = %format!("{error:#}"), "no Lighter account to trade yet");
        }
        Ok(half)
    }

    /// The trader, once the station has an account: the first time, the
    /// station registers this start's key in it.
    async fn trader(&self) -> Result<tokio::sync::MappedMutexGuard<'_, (u64, lighter::Trader)>> {
        let mut held = self.trader.lock().await;
        if held.is_none() {
            let account = self
                .chain
                .read(
                    self.exchange,
                    IExchange::addressToAccountIndexCall {
                        owner: self.station,
                    },
                )
                .await?
                .to::<u64>();
            if account == 0 {
                bail!("the station has no Lighter account yet: move dollars to it first");
            }
            self.key
                .send(
                    self.station,
                    IStationRh::approveKeyCall {
                        driver: name(LIGHTER),
                        key: self.key.address,
                    },
                )
                .await
                .context("having the station register this start's Lighter key")?;
            let markets = self.driver_markets().await?;
            let trader = lighter::Trader::new(&self.lighter_key, account, self.chain_id, markets)?;
            *held = Some((account, trader));
        }
        Ok(tokio::sync::MutexGuard::map(held, |held| {
            held.as_mut().expect("just made")
        }))
    }

    /// The markets the station's pinned driver lists, read from the chain,
    /// as the exchange writes them: the only markets this key signs in.
    async fn driver_markets(&self) -> Result<lighter::Markets> {
        let listed: Vec<(u16, u8, u8)> = self
            .chain
            .read(self.driver, ILighterDriver::marketsCall {})
            .await
            .context("reading the markets the station's Lighter driver lists")?
            .into_iter()
            .map(|market| (market.index, market.sizeDecimals, market.priceDecimals))
            .collect();
        let markets = self.api.markets().await?.listed_by_driver(&listed)?;
        if markets.iter().count() < listed.len() {
            tracing::warn!(
                station = %self.station,
                listed = ?listed,
                "the station's Lighter driver lists markets the exchange doesn't trade; nothing is signed in them"
            );
        }
        Ok(markets)
    }

    pub async fn submit(&self, payload: &[u8]) -> Outcome<Vec<u8>> {
        let action: lighter::payload::Action = match strategy_sdk::from_payload(payload) {
            Ok(action) => action,
            Err(error) => {
                return Outcome::Refused(format!("that is not a Lighter action: {error}"));
            }
        };
        let held = match self.trader().await {
            Ok(held) => held,
            Err(error) => return Outcome::Refused(format!("{error:#}")),
        };
        let (account, trader) = (&held.0, &held.1);
        let nonce = match self
            .api
            .next_nonce(*account, lighter::trader::KEY_SLOT)
            .await
        {
            Ok(nonce) => nonce,
            Err(error) => return Outcome::Refused(format!("{error:#}")),
        };
        let signed = match trader.sign(&action, nonce, now_ms()) {
            Ok(signed) => signed,
            Err(error) => return Outcome::Refused(format!("{error:#}")),
        };
        match self.api.send_tx(signed.tx_type, &signed.tx_info).await {
            Ok(tx_hash) => {
                Outcome::Accepted(strategy_sdk::to_payload(&lighter::payload::Receipt::Sent {
                    tx_hash,
                }))
            }
            // Refused by the exchange, or turned away before it: nothing
            // was done.
            Err(error) if lighter::api::not_taken(&error) => Outcome::Refused(format!("{error:#}")),
            // The exchange may have taken it before the answer was lost.
            Err(error) => Outcome::Unknown(format!("{error:#}")),
        }
    }

    pub async fn state(&self) -> Result<Vec<u8>> {
        let account = {
            let held = self.trader().await?;
            held.0
        };
        let raw = self.api.account(account).await?;
        let decimal = |value: &serde_json::Value| -> lighter::payload::Decimal {
            value.as_str().unwrap_or("0").parse().unwrap_or_default()
        };
        let positions = raw["positions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|position| {
                let size = decimal(&position["position"]);
                if size.is_zero() {
                    return None;
                }
                let signed = if position["sign"].as_i64() == Some(-1) {
                    -size
                } else {
                    size
                };
                Some(lighter::payload::Position {
                    market: position["symbol"].as_str().unwrap_or_default().to_string(),
                    size: signed,
                    entry_price: decimal(&position["avg_entry_price"]),
                    unrealized_pnl: decimal(&position["unrealized_pnl"]),
                })
            })
            .collect();
        Ok(strategy_sdk::to_payload(&lighter::payload::AccountState {
            account_index: account,
            equity: decimal(&raw["total_asset_value"]),
            available: decimal(&raw["available_balance"]),
            positions,
        }))
    }

    /// What the post reads on-chain, all at the chain's newest block: that
    /// block's time, which stamps the post, the station's pending balance at
    /// the exchange, and its own counters, so the driver can tell what moved
    /// after the reading.
    async fn on_chain(&self) -> Result<lighter::report::OnChain> {
        let (head, as_of) = self.chain.head_and_time().await?;
        let at = alloy::eips::BlockId::number(head);
        let pending = self
            .chain
            .read_at(
                self.exchange,
                IExchange::getPendingBalanceCall {
                    owner: self.station,
                    assetIndex: 3,
                },
                at,
            )
            .await?;
        let arrived = self
            .chain
            .read_at(self.station, IStationRh::arrivalsCall {}, at)
            .await?;
        let flows = self
            .chain
            .read_at(
                self.station,
                IStationRh::venueFlowsCall {
                    driver: name(LIGHTER),
                },
                at,
            )
            .await?;
        Ok(lighter::report::OnChain {
            as_of,
            pending: U256::from(pending),
            arrived,
            sent: flows.sent,
        })
    }

    /// Read the station's accounts and post them on the board every
    /// [`REPORT_EVERY`], and at once when a close staged at the station has
    /// passed its settle delay and no post read since then has been made
    /// (only such a post can prove it), until the task is dropped. A post
    /// that fails is tried again after [`RETRY_AFTER`]; no post waits on
    /// one before it for longer than [`POST_TIMEOUT`].
    pub fn report_loop(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let half = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(STAGE_LOOK_EVERY);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // When the last post was made or tried, and the chain time it
            // was read at.
            let mut last_try: Option<tokio::time::Instant> = None;
            let mut last_failed = false;
            let mut posted_as_of = 0u64;
            let mut warned = crate::Throttle::default();
            loop {
                tick.tick().await;
                let since = last_try.map(|at| at.elapsed());
                let scheduled = match since {
                    None => true,
                    Some(since) if last_failed => since >= RETRY_AFTER,
                    Some(since) => since >= REPORT_EVERY,
                };
                let proving = !scheduled
                    && !(last_failed && since.is_some_and(|since| since < RETRY_AFTER))
                    && half.stage_waits_for_a_post(posted_as_of).await;
                if !scheduled && !proving {
                    continue;
                }
                last_try = Some(tokio::time::Instant::now());
                match tokio::time::timeout(POST_TIMEOUT, half.post()).await {
                    Ok(Ok(Some(as_of))) => {
                        posted_as_of = as_of;
                        last_failed = false;
                    }
                    Ok(Ok(None)) => last_failed = false,
                    Ok(Err(error)) => {
                        last_failed = true;
                        if warned.due(std::time::Instant::now()) {
                            tracing::warn!(station = %half.station, error = %format!("{error:#}"), "the Lighter read wasn't posted");
                        }
                    }
                    Err(_) => {
                        last_failed = true;
                        if warned.due(std::time::Instant::now()) {
                            tracing::warn!(station = %half.station, "the Lighter read wasn't posted: it took more than {} s", POST_TIMEOUT.as_secs());
                        }
                    }
                }
            }
        })
    }

    /// Whether a close is staged at the station, its settle delay has
    /// passed by the chain's clock, and the newest post (read at
    /// `posted_as_of`) was read before then, so it can't prove it.
    async fn stage_waits_for_a_post(&self, posted_as_of: u64) -> bool {
        use chain::bindings::station::IStation;
        let Ok(stage) = self.chain.read(self.station, IStation::stageCall {}).await else {
            return false;
        };
        if stage.stagedAt == 0 {
            return false;
        }
        let due = stage.stagedAt.saturating_add(stage.delay);
        if posted_as_of >= due {
            return false;
        }
        match self.chain.head_and_time().await {
            Ok((_, now)) => now >= due,
            Err(_) => false,
        }
    }

    /// One attested read of every account the station owns, posted: the
    /// chain time it was read at, or `None` when the station has no account
    /// to read. Any figure that can't be read, or reads too far apart in
    /// time, and nothing is posted.
    pub async fn post(&self) -> Result<Option<u64>> {
        let account = self
            .chain
            .read(
                self.exchange,
                IExchange::addressToAccountIndexCall {
                    owner: self.station,
                },
            )
            .await?
            .to::<u64>();
        if account == 0 {
            return Ok(None);
        }
        let started = tokio::time::Instant::now();
        let started_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or_default();
        // Every market the exchange trades, not only the driver's: a
        // position in one the driver doesn't list must be posted, so the
        // driver sees it.
        let markets = self.api.markets().await?;
        let owned = self.api.accounts_of(&self.station.to_string()).await?;
        let mut accounts: Vec<serde_json::Value> = Vec::new();
        let listed = owned["sub_accounts"]
            .as_array()
            .ok_or_else(|| anyhow!("the station's accounts can't be read: {owned}"))?;
        let mut indices = Vec::new();
        for summary in listed {
            let index = summary["index"]
                .as_u64()
                .ok_or_else(|| anyhow!("one of the station's accounts has no index: {summary}"))?;
            indices.push(index);
        }
        indices.push(account);
        for index in indices {
            if accounts
                .iter()
                .any(|known| known["index"].as_u64() == Some(index))
            {
                continue;
            }
            accounts.push(self.api.account(index).await?);
        }
        let mut held = std::collections::BTreeSet::new();
        for account in &accounts {
            for position in account["positions"].as_array().into_iter().flatten() {
                // Anything but a size that reads as nothing is held: one
                // that can't be read stops the post when it is built.
                let flat = position["position"]
                    .as_str()
                    .and_then(|size| size.parse::<lighter::payload::Decimal>().ok())
                    .is_some_and(|size| size.is_zero());
                if let (false, Some(market)) = (flat, position["market_id"].as_u64())
                    && let Ok(market) = u16::try_from(market)
                {
                    held.insert(market);
                }
            }
        }
        let mut prices = Vec::new();
        for market in held {
            let details = self.api.book_details(market).await?;
            let book = self.api.book_orders(market, 1).await?;
            prices.push(lighter::report::prices(market, &details, &book, &markets)?);
        }
        let mut pools = Vec::new();
        for pool in lighter::report::pools_held(&accounts) {
            pools.push(self.api.account(pool).await?);
        }
        let on_chain = self.on_chain().await?;
        read_together(started.elapsed(), started_at, on_chain.as_of)?;
        let report = lighter::report::build(on_chain, &accounts, &prices, &markets, &pools)?;
        tracing::info!(report = %lighter::report::describe(self.station, &report), "posting the Lighter read");
        self.key
            .send(
                self.board,
                lighter::report::ILighterBoard::postCall {
                    station: self.station,
                    report,
                },
            )
            .await
            .map(|_| Some(on_chain.as_of))
            .map_err(|error| anyhow!("{error:#}"))
    }
}

/// Whether a post's API reads (begun `started_at`, by the machine's clock,
/// and taking `took` up to the end of the chain read) and its chain read (at
/// a block of time `as_of`) are close enough to be posted as one reading.
pub fn read_together(took: Duration, started_at: u64, as_of: u64) -> Result<()> {
    if took > POST_READ_SPAN {
        bail!(
            "the exchange's API and the chain were read {} s apart, more than {} s: nothing is posted",
            took.as_secs(),
            POST_READ_SPAN.as_secs()
        );
    }
    if as_of.saturating_add(POST_READ_SPAN.as_secs()) < started_at {
        bail!(
            "the chain's newest block is {} s older than the exchange's API read, more than {} s: nothing is posted",
            started_at - as_of,
            POST_READ_SPAN.as_secs()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_post_is_made_only_of_reads_close_together_in_time() {
        let span = POST_READ_SPAN.as_secs();
        assert!(read_together(Duration::from_secs(3), 1_000, 1_002).is_ok());
        assert!(read_together(POST_READ_SPAN, 1_000, 1_000).is_ok());
        // The reads took too long from the first API read to the chain's.
        assert!(read_together(POST_READ_SPAN + Duration::from_secs(1), 1_000, 1_000).is_err());
        // The chain's newest block predates the API read by too much.
        assert!(read_together(Duration::from_secs(1), 1_000 + span + 1, 1_000).is_err());
        assert!(read_together(Duration::from_secs(1), 1_000 + span, 1_000).is_ok());
    }
}
