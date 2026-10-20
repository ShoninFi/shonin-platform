//! Solana devnet: Orca's pool the Orca driver lists (wrapped SOL against
//! Circle's devnet USDC), kept at Pyth's SOL/USD price as the cluster holds
//! it, the price the driver values a station at.
//!
//! An Orca pool keeps its price as Uniswap v3 does, the square root of B
//! per A on the same ticks (1.0001 to the tick), only in Q64.64 where
//! Uniswap uses Q64.96, and its fee is in millionths as Uniswap's is. So the
//! pool is read into the EVM side's [`PoolState`] (its square root shifted
//! by 32 bits), Pyth's price is the feed's answer, the initialized ticks of
//! the three tick arrays a swap is given are the ticks it may cross, and
//! [`decide`] judges it under the same band, cap, floors and buy-back as
//! every EVM pool. The swap is Orca's own, exact in, with the target as its
//! price limit, so it never carries the pool past the price.
//!
//! SOL is held both wrapped and as the account's coin: the coin above
//! [`GAS_KEEP`] counts as SOL to sell, and is wrapped in the swap's own
//! transaction when it is sold.
//!
//! The pool's depth is the account's own position ("the seed"), opened by
//! `testnet-market-setup` across the price, [`SEED_HALF_WIDTH_BPS`] either
//! side, with what a faucet gives. Once Pyth's price leaves the seed's inner
//! half, the seed is taken out whole, the pool brought to the price, and the
//! seed opened again around it with as much as it held ([`SolanaMarket::keep`]).

use std::sync::Mutex;
use std::time::Instant;

use alloy::primitives::U256;
use anyhow::{Context, Result, anyhow, bail};
use chain::solana::orca::{self as venue, PoolNow, setup};
use chain::solana::programs::anchor_spl::associated_token::{
    get_associated_token_address, spl_associated_token_account::instruction,
};
use chain::solana::programs::anchor_spl::token::spl_token;
use chain::solana::{Instruction, Keypair, Pubkey, Signer, SolanaClient};

use crate::health::{Health, PoolHealth};
use crate::homes::Homes;
use crate::market::{
    Decision, Settings, Situation, decide, now, price_of, record_pool, units, whole,
};
use crate::math::{self, Pair, PoolState};

/// The token the listed pool pairs with the dollar, and the dollar.
pub const SOL: &str = "SOL";
pub const USDC: &str = "USDC";

/// The account's coin kept for fees and its accounts' rent: never sold.
pub const GAS_KEEP: u64 = 100_000_000;

/// Below this much coin, every round says so at warn.
pub const LOW_GAS: u64 = 200_000_000;

/// How far either side of the price the seed reaches, in basis points.
pub const SEED_HALF_WIDTH_BPS: u128 = 500;

/// One dollar a SOL as the chain crate counts prices.
const PRICE_ONE: u128 = 10u128.pow(venue::PRICE_DECIMALS);

/// Pyth's price as the chain crate counts prices (dollars per SOL times
/// 10^12).
pub fn price12(answer: U256, feed_decimals: u8) -> u128 {
    let answer = u128::try_from(answer).unwrap_or(u128::MAX);
    let exponent = venue::PRICE_DECIMALS as i32 - i32::from(feed_decimals);
    if exponent >= 0 {
        answer.saturating_mul(10u128.pow(exponent as u32))
    } else {
        answer / 10u128.pow(exponent.unsigned_abs())
    }
}

/// Pyth's price as a feed's answer and its decimals: `None` for a price
/// that isn't positive or an exponent no feed has.
pub fn pyth_answer(price: u64, exponent: i32) -> Option<(U256, u8)> {
    if price == 0 || exponent > 0 {
        return None;
    }
    let decimals = u8::try_from(exponent.unsigned_abs()).ok()?;
    Some((U256::from(price), decimals))
}

/// How the listed pool lines up with Pyth's price: which of its tokens is
/// SOL, and the decimals.
pub fn pair(feed_decimals: u8) -> Pair {
    Pair {
        token_is_token0: venue::asset_is_a(),
        token_decimals: venue::ASSET_DECIMALS as u8,
        dollar_decimals: venue::DOLLAR_DECIMALS as u8,
        feed_decimals,
    }
}

/// An Orca square-root price (Q64.64) as Uniswap's (Q64.96).
pub fn to_x96(sqrt_price: u128) -> U256 {
    U256::from(sqrt_price) << 32
}

/// A Uniswap square-root price as Orca's, rounded down, within the bounds
/// an Orca pool accepts.
pub fn to_x64(sqrt_price: U256) -> u128 {
    let raw = u128::try_from(sqrt_price >> 32).unwrap_or(u128::MAX);
    raw.clamp(
        shonin_orca_math::MIN_SQRT_PRICE,
        shonin_orca_math::MAX_SQRT_PRICE,
    )
}

/// Orca's square-root price bounds, as its program has them.
mod shonin_orca_math {
    pub use chain::solana::programs::orca_driver::math::{MAX_SQRT_PRICE, MIN_SQRT_PRICE};
}

/// The listed pool's state in the EVM side's terms.
pub fn state_of(pool: &PoolNow) -> PoolState {
    PoolState {
        sqrt_price: to_x96(pool.state.sqrt_price),
        tick: pool.state.tick_current,
        liquidity: pool.state.liquidity,
        fee: u32::from(pool.state.fee_rate),
    }
}

/// Of the initialized ticks `ticks` (lowest first, from the arrays a swap is
/// given), those a swap from `tick` to `target_tick` crosses, in the order
/// it meets them, and how far the arrays reach (`starts` their first ticks,
/// each `span` ticks long): a swap goes no further.
pub fn crossed(
    ticks: &[(i32, i128)],
    tick: i32,
    target_tick: i32,
    starts: &[i32],
    span: i32,
) -> (Vec<(i32, i128)>, Option<i32>) {
    let down = target_tick < tick;
    let mut found: Vec<(i32, i128)> = if down {
        ticks
            .iter()
            .copied()
            .filter(|(at, _)| *at <= tick && *at > target_tick)
            .collect()
    } else {
        ticks
            .iter()
            .copied()
            .filter(|(at, _)| *at > tick && *at <= target_tick)
            .collect()
    };
    found.sort_by_key(|(at, _)| *at);
    if down {
        found.reverse();
    }
    let reach = if down {
        starts.iter().min().copied()
    } else {
        starts.iter().max().map(|start| start + span)
    };
    (found, reach)
}

/// The Solana account's SOL as the market counts it: what it holds wrapped,
/// and its coin above [`GAS_KEEP`].
pub fn sol_held(wrapped: u64, lamports: u64) -> u64 {
    wrapped.saturating_add(lamports.saturating_sub(GAS_KEEP))
}

/// Solana devnet's pool, and the account that keeps it.
pub struct SolanaMarket {
    pub key: String,
    pub label: String,
    pub client: SolanaClient,
    pub signer: Keypair,
    /// When the pool was last said at warn to be stuck one way.
    warned: std::collections::BTreeMap<bool, Instant>,
}

impl SolanaMarket {
    /// The cluster `client` speaks to, kept by `signer`; refused unless the
    /// pool the build lists is there, is Orca's, and pairs wrapped SOL with
    /// the dollar the build lists.
    pub async fn new(
        key: String,
        label: String,
        client: SolanaClient,
        signer: Keypair,
    ) -> Result<Self> {
        let pool = read_pool(&client).await?;
        let (a, b) = chain::solana::programs::orca_driver::network::mints();
        if pool.state.mint_a != a || pool.state.mint_b != b {
            bail!(
                "Orca's pool {} pairs {} with {}, not the build's {} with {}",
                pool.address,
                pool.state.mint_a,
                pool.state.mint_b,
                a,
                b
            );
        }
        Ok(Self {
            key,
            label,
            client,
            signer,
            warned: Default::default(),
        })
    }

    /// The account's address.
    pub fn account(&self) -> Pubkey {
        self.signer.pubkey()
    }

    /// One round: the pool brought towards Pyth's price, if it needs it.
    pub async fn keep(
        &mut self,
        settings: &Settings,
        homes: &mut Homes,
        health: &Mutex<Health>,
    ) -> Result<()> {
        let who = self.account();
        let lamports = self.client.lamports(&who).await?;
        if lamports < LOW_GAS {
            tracing::warn!(chain = %self.key, gas = %units(U256::from(lamports), 9), "the account is low on SOL: top it up from the deployer");
        }
        if let Ok(mut shared) = health.lock() {
            let chain = shared.chain(&self.key);
            chain.label = self.label.clone();
            chain.gas = units(U256::from(lamports), 9);
            chain.error = None;
        }

        let account = self
            .client
            .account(&venue::PRICE_ACCOUNT)
            .await?
            .ok_or_else(|| anyhow!("Pyth's SOL/USD account is not on {}", self.key))?;
        let price = venue::read_price(&account)?;
        let (answer, feed_decimals) =
            pyth_answer(price.price, price.exponent).ok_or_else(|| {
                anyhow!(
                    "Pyth's SOL/USD price reads {}e{}",
                    price.price,
                    price.exponent
                )
            })?;
        let pair = pair(feed_decimals);
        let target = pair
            .sqrt_price_at(answer)
            .ok_or_else(|| anyhow!("Pyth's price is outside any pool's range"))?;
        let pool = read_pool(&self.client).await?;
        let state = state_of(&pool);
        let premium = math::token_premium_bps(&pair, state.sqrt_price, target);
        let reference = math::to_f64(answer) / 10f64.powi(i32::from(feed_decimals));
        let published = u64::try_from(price.publish_time).unwrap_or_default();
        let mut line = PoolHealth {
            symbol: SOL.to_string(),
            pool: pool.address.to_string(),
            reference,
            pool_price: price_of(&pair, state.sqrt_price),
            premium_bps: premium,
            feed_updated_at: published,
            last_trade: None,
            stuck: None,
        };
        let age = self.client.now().await?.saturating_sub(published);
        if age > venue::MAX_AGE {
            tracing::warn!(chain = %self.key, age_seconds = age, "Pyth's price is older than the driver takes: the pool is left as it is");
            record_pool(health, &self.key, line);
            return Ok(());
        }

        // The seed, taken out whole once Pyth has left its inner half, and
        // opened again around the price once the pool is brought there.
        let pyth = price12(answer, feed_decimals);
        let mut reopen = None;
        if !settings.dry_run
            && let Some(seed) = setup::seed_position(&self.client, &self.signer).await?
            && seed.liquidity > 0
            && setup::needs_recentring(&seed, pyth)
        {
            let (a, b) = venue::holds(&seed, pool.state.sqrt_price)?;
            let (sol, dollars) = if venue::asset_is_a() { (a, b) } else { (b, a) };
            let at = venue::price_of_sqrt(pool.state.sqrt_price);
            // Half its worth, in dollar-units (lamports times the price).
            let worth = u128::from(dollars) + u128::from(sol) * at / PRICE_ONE / 1_000;
            withdraw_seed(&self.client, &self.signer).await?;
            tracing::info!(chain = %self.key, pyth = pyth as f64 / PRICE_ONE as f64, "Pyth left the seed's inner half: the seed is taken out, to be opened again around the price");
            reopen = Some(u64::try_from(worth / 2).unwrap_or(u64::MAX));
        }
        let pool = read_pool(&self.client).await?;
        let state = state_of(&pool);

        // No liquidity at the pool's price (the seed just taken out, or the
        // price moved past every position): no swap plan reaches the price
        // there, so the pool is walked to it, a capped swap at a time, and
        // the seed opened again around it.
        let off = math::token_premium_bps(&pair, state.sqrt_price, target).abs();
        if reopen.is_some() || (state.liquidity == 0 && off >= settings.band_bps) {
            if !settings.dry_run {
                let cap_dollars = (settings.max_trade_usd.max(0.0) * 1e6) as u64;
                let cap_sol = (u128::from(cap_dollars) * 1_000 * PRICE_ONE / pyth.max(1)) as u64;
                match walk_to_pyth(
                    &self.client,
                    &self.signer,
                    settings.band_bps,
                    3,
                    Some((cap_dollars, cap_sol)),
                )
                .await
                {
                    Ok(at) => {
                        line.last_trade = Some(now());
                        line.pool_price = at;
                        line.premium_bps = (at / reference - 1.0) * 10_000.0;
                        tracing::info!(chain = %self.key, pool = at, pyth = reference, "walked the pool to Pyth's price");
                    }
                    Err(error) => {
                        line.stuck = Some(format!("{error:#}"));
                        tracing::warn!(chain = %self.key, error = format!("{error:#}"), "the pool could not be walked to Pyth's price");
                    }
                }
                if let Some(dollars) = reopen {
                    open_seed(&self.client, &self.signer, dollars)
                        .await
                        .context("opening the seed again around the price")?;
                }
            }
            record_pool(health, &self.key, line);
            return Ok(());
        }

        // The ticks the swap may cross: those of the arrays it is given.
        let a_to_b = target < state.sqrt_price;
        let starts = venue::swap_starts(&pool.state, a_to_b);
        let addresses: Vec<Pubkey> = starts
            .iter()
            .map(|start| venue::tick_array_address(&pool.address, *start))
            .collect();
        let arrays = self.client.rpc.accounts(&addresses).await?;
        let mut ticks = Vec::new();
        for (address, account) in addresses.iter().zip(&arrays) {
            ticks.extend(venue::initialized_ticks(
                address,
                account.as_ref(),
                pool.state.tick_spacing,
            )?);
        }
        let span = venue::TICKS_PER_ARRAY * i32::from(pool.state.tick_spacing);
        let (crossing, reach) = crossed(
            &ticks,
            state.tick,
            math::tick_at_sqrt_ratio(target),
            &starts,
            span,
        );
        let Some(plan) = math::plan_to(&state, target, &crossing, reach) else {
            record_pool(health, &self.key, line);
            return Ok(());
        };

        let wrapped = self.client.token_balance(&venue::wsol_of(&who)).await?;
        let dollars_account = get_associated_token_address(&who, &venue::DOLLAR_MINT);
        let dollars = self.client.token_balance(&dollars_account).await?;
        let sol = sol_held(wrapped, lamports);
        let floor = |symbol: &str, decimals: u32| {
            whole(
                settings.floors.get(symbol).copied().unwrap_or_default(),
                decimals as u8,
            )
        };
        let token_balance = U256::from(sol);
        let dollar_balance = U256::from(dollars);
        let situation = Situation {
            pair,
            pool: state,
            target,
            plan,
            answer,
            token_balance,
            dollar_balance,
            token_floor: floor(SOL, venue::ASSET_DECIMALS),
            dollar_floor: floor(USDC, venue::DOLLAR_DECIMALS),
            token_home: Some(homes.home(&Homes::key(&self.key, SOL), token_balance)),
            dollar_home: Some(homes.home(
                &Homes::key(&self.key, &venue::DOLLAR_MINT.to_string()),
                dollar_balance,
            )),
            dollar_mintable: false,
        };
        match decide(settings, &situation) {
            Decision::InBand => {}
            Decision::Stuck { zero_for_one } => {
                let buying = zero_for_one == pair.zero_for_one_spends_dollar();
                let (way, asset) = if buying { ("up", USDC) } else { ("down", SOL) };
                line.stuck = Some(format!("can't move it {way}: no {asset} above the floor"));
                if self.warn_due(settings, zero_for_one) {
                    tracing::warn!(chain = %self.key, token = SOL, premium_bps = format!("{premium:.1}"), way, asset, sol = %units(token_balance, 9), usdc = %units(dollar_balance, 6), "can't move the pool: nothing above the floor to sell into it");
                }
            }
            Decision::Swap {
                zero_for_one,
                amount_in,
                short,
            } => {
                let buying = zero_for_one == pair.zero_for_one_spends_dollar();
                let (asset_in, decimals_in) = if buying { (USDC, 6) } else { (SOL, 9) };
                if short {
                    let way = if buying { "up" } else { "down" };
                    line.stuck = Some(format!(
                        "moving it {way} only part of the way: short of {asset_in}"
                    ));
                    if self.warn_due(settings, zero_for_one) {
                        tracing::warn!(chain = %self.key, token = SOL, premium_bps = format!("{premium:.1}"), way, asset = asset_in, "can't move the pool all the way: not enough above the floor");
                    }
                }
                let amount = u64::try_from(amount_in).unwrap_or(u64::MAX);
                if settings.dry_run {
                    tracing::info!(chain = %self.key, token = SOL, side = if buying { "buy" } else { "sell" }, premium_bps = format!("{premium:.1}"), amount_in = %units(amount_in, decimals_in), asset_in, "would swap (dry run)");
                } else {
                    let ixs =
                        swap_ixs(&who, &pool, buying, amount, wrapped, to_x64(target), a_to_b)?;
                    setup::ensure_arrays(&self.client, &self.signer, &pool, &starts)
                        .await
                        .context("making the tick arrays the swap crosses")?;
                    let landed = self
                        .client
                        .send(ixs, &[&self.signer])
                        .await
                        .context("swapping in Orca's pool")?;
                    let after = read_pool(&self.client).await?;
                    let after_state = state_of(&after);
                    let premium_after =
                        math::token_premium_bps(&pair, after_state.sqrt_price, target);
                    tracing::info!(
                        chain = %self.key,
                        token = SOL,
                        side = if buying { "buy" } else { "sell" },
                        paid = %units(amount_in, decimals_in),
                        asset_in,
                        premium_bps_before = format!("{premium:.1}"),
                        premium_bps_after = format!("{premium_after:.1}"),
                        reference,
                        transaction = %landed.record.signature,
                        fee_lamports = landed.record.fee,
                        "swapped the pool towards Pyth's price"
                    );
                    line.last_trade = Some(now());
                    line.premium_bps = premium_after;
                    line.pool_price = price_of(&pair, after_state.sqrt_price);
                }
            }
        }
        record_pool(health, &self.key, line);
        Ok(())
    }

    /// Whether the pool stuck one way is to be said at warn again now: once
    /// every `warn_every`.
    fn warn_due(&mut self, settings: &Settings, zero_for_one: bool) -> bool {
        let due = self
            .warned
            .get(&zero_for_one)
            .is_none_or(|at| at.elapsed() >= settings.warn_every);
        if due {
            self.warned.insert(zero_for_one, Instant::now());
        }
        due
    }
}

/// The listed pool as the cluster holds it.
pub async fn read_pool(client: &SolanaClient) -> Result<PoolNow> {
    let address = venue::POOLS[0].address;
    let account = client
        .account(&address)
        .await?
        .ok_or_else(|| anyhow!("Orca's pool {address} is not on {}", client.key))?;
    Ok(PoolNow {
        address,
        state: venue::read_pool(&address, &account)?,
    })
}

/// The swap's transaction: the account's two token accounts made if they
/// aren't there, the SOL it sells wrapped first where its wrapped SOL falls
/// short, then Orca's swap of `amount` in, no further than `limit`.
pub fn swap_ixs(
    who: &Pubkey,
    pool: &PoolNow,
    buying: bool,
    amount: u64,
    wrapped: u64,
    limit: u128,
    a_to_b: bool,
) -> Result<Vec<Instruction>> {
    let dollars = get_associated_token_address(who, &venue::DOLLAR_MINT);
    let mut ixs = vec![
        venue::ix_open_wsol(who, who),
        instruction::create_associated_token_account_idempotent(
            who,
            who,
            &venue::DOLLAR_MINT,
            &spl_token::ID,
        ),
    ];
    if !buying && wrapped < amount {
        // Opened just above; the rest of the wrap.
        ixs.extend(setup::ixs_wrap(who, amount - wrapped)?.into_iter().skip(1));
    }
    ixs.push(venue::ix_swap(
        who,
        pool,
        venue::owners(who, &dollars),
        amount,
        0,
        limit,
        a_to_b,
    ));
    Ok(ixs)
}

/// The account's dollar account (Circle's USDC).
pub fn dollars_of(who: &Pubkey) -> Pubkey {
    get_associated_token_address(who, &venue::DOLLAR_MINT)
}

/// The seed opened across the pool's price, [`SEED_HALF_WIDTH_BPS`] either
/// side, with up to `dollars` of USDC and the SOL that goes with them at
/// that price, both bounded by what the account holds above its floors:
/// its position's mint is derived from its own key, so it is always the
/// same one. A seed already funded is left alone.
pub async fn open_seed(client: &SolanaClient, signer: &Keypair, dollars: u64) -> Result<()> {
    let existing = setup::seed_position(client, signer).await?;
    if existing.as_ref().is_some_and(|seed| seed.liquidity > 0) {
        return Ok(());
    }
    let who = signer.pubkey();
    let pool = read_pool(client).await?;
    let price = venue::price_of_sqrt(pool.state.sqrt_price);
    let ticks = match &existing {
        Some(seed) => (seed.tick_lower, seed.tick_upper),
        None => venue::ticks_between(
            price * (10_000 - SEED_HALF_WIDTH_BPS) / 10_000,
            price * (10_000 + SEED_HALF_WIDTH_BPS) / 10_000,
            pool.state.tick_spacing,
        )?,
    };
    // What it holds, less what is never put in.
    let held_dollars = client.token_balance(&dollars_of(&who)).await?;
    let wrapped = client.token_balance(&venue::wsol_of(&who)).await?;
    let sol_free =
        sol_held(wrapped, client.lamports(&who).await?).saturating_sub(SEED_RENT + 50_000_000);
    let dollars = dollars.min(held_dollars.saturating_sub(5_000_000));
    // The SOL worth the dollars at the pool's price, and a tenth more, so
    // the dollar side is the one used up.
    let sol = (u128::from(dollars) * 1_000 * PRICE_ONE / price.max(1) * 11 / 10) as u64;
    let sol = sol.min(sol_free);
    if dollars == 0 || sol == 0 {
        bail!(
            "the account ({who}) holds {} USDC and {} SOL above its floors: nothing to open the seed with",
            held_dollars as f64 / 1e6,
            sol_free as f64 / 1e9
        );
    }
    // Both ends' arrays, once each: the seed often fits in one.
    let mut starts = vec![
        venue::array_start(ticks.0, pool.state.tick_spacing),
        venue::array_start(ticks.1, pool.state.tick_spacing),
    ];
    starts.dedup();
    setup::ensure_arrays(client, signer, &pool, &starts).await?;
    let mut ixs = vec![instruction::create_associated_token_account_idempotent(
        &who,
        &who,
        &venue::DOLLAR_MINT,
        &spl_token::ID,
    )];
    if wrapped < sol {
        ixs.extend(setup::ixs_wrap(&who, sol - wrapped)?);
    } else {
        ixs.push(venue::ix_open_wsol(&who, &who));
    }
    client
        .send(ixs, &[signer])
        .await
        .context("readying the seed's tokens")?;
    let mint = setup::seed_mint(signer);
    if existing.is_none() {
        client
            .send(
                vec![venue::ix_open_position(
                    &who,
                    &who,
                    &mint.pubkey(),
                    &pool.address,
                    ticks,
                )],
                &[signer, &mint],
            )
            .await
            .context("opening the seed's position")?;
    }
    let (max_a, max_b) = if venue::asset_is_a() {
        (sol, dollars)
    } else {
        (dollars, sol)
    };
    let liquidity = venue::liquidity_for(
        pool.state.tick_current,
        pool.state.sqrt_price,
        ticks,
        max_a,
        max_b,
    )?;
    if liquidity == 0 {
        bail!("the seed's amounts buy no liquidity");
    }
    let position = existing.unwrap_or(venue::Position {
        whirlpool: pool.address,
        mint: mint.pubkey(),
        liquidity: 0,
        tick_lower: ticks.0,
        tick_upper: ticks.1,
        checkpoint_a: 0,
        fee_owed_a: 0,
        checkpoint_b: 0,
        fee_owed_b: 0,
    });
    client
        .send(
            vec![venue::ix_modify(
                venue::Change::Increase,
                &who,
                &pool,
                &position,
                &mint.pubkey(),
                venue::owners(&who, &dollars_of(&who)),
                liquidity,
                (max_a, max_b),
            )],
            &[signer],
        )
        .await
        .context("funding the seed")?;
    tracing::info!(
        sol = sol as f64 / 1e9,
        usdc = dollars as f64 / 1e6,
        price = price as f64 / PRICE_ONE as f64,
        "the seed is open across the pool's price"
    );
    Ok(())
}

/// What a seed's position and its token account cost in rent, about: kept
/// back from the SOL it is opened with.
const SEED_RENT: u64 = 20_000_000;

/// The seed taken out whole, its fees collected and its position closed
/// (its rent back to the account). Nothing if it isn't open.
pub async fn withdraw_seed(client: &SolanaClient, signer: &Keypair) -> Result<()> {
    let Some(position) = setup::seed_position(client, signer).await? else {
        return Ok(());
    };
    let pool = read_pool(client).await?;
    let who = signer.pubkey();
    let mint = setup::seed_mint(signer).pubkey();
    let owners = venue::owners(&who, &dollars_of(&who));
    let mut ixs = vec![
        venue::ix_open_wsol(&who, &who),
        instruction::create_associated_token_account_idempotent(
            &who,
            &who,
            &venue::DOLLAR_MINT,
            &spl_token::ID,
        ),
    ];
    if position.liquidity > 0 {
        ixs.push(venue::ix_modify(
            venue::Change::Decrease,
            &who,
            &pool,
            &position,
            &mint,
            owners,
            position.liquidity,
            (0, 0),
        ));
    }
    ixs.push(venue::ix_collect_fees(&who, &pool, &mint, owners));
    ixs.push(venue::ix_close_position(&who, &who, &mint));
    client
        .send(ixs, &[signer])
        .await
        .context("taking the seed out")?;
    Ok(())
}

/// The pool walked to Pyth's price (within `band_bps`) with the account's
/// own tokens, however far it is: a pool nobody has kept can stand far from
/// it with little or no liquidity between. Each step is one swap no further
/// than the tick arrays it is given reach (made where they aren't there),
/// at most `steps` of them, each spending at most `cap` (dollar-units,
/// lamports) where one is given. Says the pool's price after.
pub async fn walk_to_pyth(
    client: &SolanaClient,
    signer: &Keypair,
    band_bps: f64,
    steps: usize,
    cap: Option<(u64, u64)>,
) -> Result<f64> {
    let who = signer.pubkey();
    let account = client
        .account(&venue::PRICE_ACCOUNT)
        .await?
        .ok_or_else(|| anyhow!("Pyth's SOL/USD account is not on {}", client.key))?;
    let price = venue::read_price(&account)?;
    let (answer, decimals) = pyth_answer(price.price, price.exponent).ok_or_else(|| {
        anyhow!(
            "Pyth's SOL/USD price reads {}e{}",
            price.price,
            price.exponent
        )
    })?;
    let pair = pair(decimals);
    let target = pair
        .sqrt_price_at(answer)
        .ok_or_else(|| anyhow!("Pyth's price is outside any pool's range"))?;
    let mut last = None;
    for _ in 0..steps {
        let pool = read_pool(client).await?;
        let state = state_of(&pool);
        let off = math::token_premium_bps(&pair, state.sqrt_price, target);
        if off.abs() < band_bps {
            return Ok(price_of(&pair, state.sqrt_price));
        }
        if last == Some(pool.state.sqrt_price) {
            bail!(
                "the pool stays {off:.0} bps from Pyth's price: the account's tokens can't move it further"
            );
        }
        last = Some(pool.state.sqrt_price);
        let a_to_b = target < state.sqrt_price;
        let starts = venue::swap_starts(&pool.state, a_to_b);
        setup::ensure_arrays(client, signer, &pool, &starts).await?;
        // No further than the arrays reach, a step inside their edge.
        let spacing = i32::from(pool.state.tick_spacing);
        let span = venue::TICKS_PER_ARRAY * spacing;
        let edge = if a_to_b {
            starts.iter().min().copied().unwrap_or_default()
        } else {
            starts.iter().max().copied().unwrap_or_default() + span - spacing
        };
        let edge = chain::solana::programs::orca_driver::math::sqrt_price_at(edge)
            .map_err(|_| anyhow!("tick {edge} is out of range"))?;
        let goal = to_x64(target);
        let limit = if a_to_b {
            goal.max(edge)
        } else {
            goal.min(edge)
        };
        let buying = a_to_b == pair.zero_for_one_spends_dollar();
        let wrapped = client.token_balance(&venue::wsol_of(&who)).await?;
        let amount = if buying {
            let held = client
                .token_balance(&dollars_of(&who))
                .await?
                .saturating_sub(5_000_000);
            cap.map_or(held, |(dollars, _)| held.min(dollars))
        } else {
            let held = sol_held(wrapped, client.lamports(&who).await?).saturating_sub(50_000_000);
            cap.map_or(held, |(_, lamports)| held.min(lamports))
        };
        if amount == 0 {
            bail!(
                "the account ({who}) has no {} above its floor to move the pool with",
                if buying { USDC } else { SOL }
            );
        }
        client
            .send(
                swap_ixs(&who, &pool, buying, amount, wrapped, limit, a_to_b)?,
                &[signer],
            )
            .await
            .context("walking the pool to Pyth's price")?;
    }
    let pool = read_pool(client).await?;
    Ok(price_of(&pair, state_of(&pool).sqrt_price))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chain::solana::programs::orca_driver::math as orca_math;

    /// Orca's own amount of A or B that moves the price between two square
    /// roots, as a fee-less swap takes it.
    fn orca_in(from: u128, to: u128, liquidity: u128, a_in: bool) -> u64 {
        if a_in {
            orca_math::amount_a(to, from, liquidity, true).unwrap()
        } else {
            orca_math::amount_b(from, to, liquidity, true).unwrap()
        }
    }

    #[test]
    fn pyth_s_price_is_a_feed_answer_with_its_exponent_as_decimals() {
        assert_eq!(
            pyth_answer(10_499_973_822, -8),
            Some((U256::from(10_499_973_822u64), 8))
        );
        assert_eq!(pyth_answer(0, -8), None);
        assert_eq!(pyth_answer(5, 2), None);
    }

    #[test]
    fn pyth_s_price_reads_as_the_chain_crate_counts_prices() {
        assert_eq!(price12(U256::from(10_500_000_000u64), 8), 105 * PRICE_ONE);
        assert_eq!(price12(U256::from(105u64), 0), 105 * PRICE_ONE);
    }

    #[test]
    fn an_orca_square_root_reads_as_uniswap_s_and_back() {
        let sqrt = 5_970_000_000_000_000_000u128;
        assert_eq!(to_x64(to_x96(sqrt)), sqrt);
        // Pyth at $105 lands on the same price either way round.
        for order in [true, false] {
            let pair = Pair {
                token_is_token0: order,
                token_decimals: 9,
                dollar_decimals: 6,
                feed_decimals: 8,
            };
            let target = pair.sqrt_price_at(U256::from(105_00000000u64)).unwrap();
            let x64 = to_x64(target);
            let back = crate::market::price_of(&pair, to_x96(x64));
            assert!((back - 105.0).abs() < 1e-6, "{order}: {back}");
        }
    }

    #[test]
    fn the_swap_to_pyth_s_price_takes_what_orca_s_own_maths_takes_in_either_token_order() {
        // SOL at $100 in a pool of liquidity 2.3e10, Pyth at $102 or $98;
        // both token orders, SOL's 9 decimals against the dollar's 6.
        for token_is_token0 in [true, false] {
            let pair = Pair {
                token_is_token0,
                token_decimals: 9,
                dollar_decimals: 6,
                feed_decimals: 8,
            };
            let start = to_x64(pair.sqrt_price_at(U256::from(100_00000000u64)).unwrap());
            let liquidity = 23_000_000_000u128;
            let state = PoolState {
                sqrt_price: to_x96(start),
                tick: math::tick_at_sqrt_ratio(to_x96(start)),
                liquidity,
                fee: 0,
            };
            for pyth in [102_00000000u64, 98_00000000] {
                let target = pair.sqrt_price_at(U256::from(pyth)).unwrap();
                let plan = math::plan_to(&state, target, &[], None).unwrap();
                let to = to_x64(target);
                // Token0 (A) in lowers the price.
                let a_in = plan.zero_for_one;
                assert_eq!(a_in, to < start);
                let orca = orca_in(start, to, liquidity, a_in);
                let ours = u64::try_from(plan.amount_in).unwrap();
                assert!(
                    ours.abs_diff(orca) <= 2,
                    "{token_is_token0} {pyth}: {ours} against Orca's {orca}"
                );
                // A dearer SOL is bought with dollars.
                let buys_sol = plan.zero_for_one == pair.zero_for_one_spends_dollar();
                assert_eq!(buys_sol, pyth > 100_00000000);
                // In whole units, with SOL's 9 decimals and the dollar's 6:
                // liquidity 2.3e10 is about $3,600 a 1% move in the square
                // root at $100, so 2% in price is about $72 either way.
                let (decimals, worth) = if buys_sol { (6, 1.0) } else { (9, 100.0) };
                let dollars = ours as f64 / 10f64.powi(decimals) * worth;
                assert!((65.0..80.0).contains(&dollars), "{dollars}");
            }
        }
    }

    #[test]
    fn the_ticks_crossed_are_those_between_in_the_order_met_and_no_further_than_the_arrays() {
        let ticks = [(-640, 5), (-64, 7), (0, -3), (128, 9), (6_000, 1)];
        let span = 88 * 64;
        let starts = [0, span, 2 * span];
        // Up from tick 10 to 200: 128 only, and the arrays reach 3 spans up.
        let (found, reach) = crossed(&ticks, 10, 200, &starts, span);
        assert_eq!(found, vec![(128, 9)]);
        assert_eq!(reach, Some(3 * span));
        // Down from 10 to -700: 0, -64, -640, highest first; the lowest
        // array's first tick is as far as it goes.
        let down = [0, -span, -2 * span];
        let (found, reach) = crossed(&ticks, 10, -700, &down, span);
        assert_eq!(found, vec![(0, -3), (-64, 7), (-640, 5)]);
        assert_eq!(reach, Some(-2 * span));
    }

    #[test]
    fn the_coin_kept_for_fees_is_never_counted_as_sol_to_sell() {
        assert_eq!(sol_held(0, GAS_KEEP / 2), 0);
        assert_eq!(sol_held(7, GAS_KEEP + 5), 12);
    }

    #[test]
    fn sol_sold_beyond_what_is_wrapped_is_wrapped_in_the_swap_s_own_transaction() {
        let who = Pubkey::new_unique();
        let pool = PoolNow {
            address: venue::POOLS[0].address,
            state: chain::solana::programs::orca_driver::orca::Whirlpool {
                config: venue::CONFIG,
                tick_spacing: 64,
                fee_tier_index: 64,
                fee_rate: 2_000,
                protocol_fee_rate: 300,
                liquidity: 1,
                sqrt_price: 1 << 64,
                tick_current: 0,
                mint_a: Pubkey::new_unique(),
                vault_a: Pubkey::new_unique(),
                fee_growth_global_a: 0,
                mint_b: Pubkey::new_unique(),
                vault_b: Pubkey::new_unique(),
                fee_growth_global_b: 0,
            },
        };
        let selling = swap_ixs(&who, &pool, false, 100, 40, 1 << 63, true).unwrap();
        // Open both accounts, move 60 lamports in, sync, swap.
        assert_eq!(selling.len(), 5);
        assert_eq!(selling.last().unwrap().program_id, venue::WHIRLPOOL_PROGRAM);
        let buying = swap_ixs(&who, &pool, true, 100, 0, 1 << 65, false).unwrap();
        assert_eq!(buying.len(), 3, "dollars in: nothing to wrap");
        let enough = swap_ixs(&who, &pool, false, 100, 100, 1 << 63, true).unwrap();
        assert_eq!(enough.len(), 3);
    }
}
