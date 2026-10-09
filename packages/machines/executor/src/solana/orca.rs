//! The `orca` driver's executor half: what an agent's Orca actions come to,
//! what it hears of its station's holdings there, and when.
//!
//! Each action is decoded here and checked against what the station's pinned
//! Orca driver lists (its pools, and the positions at the station's own
//! slots), then sent as station calls through the guard, which checks each
//! one against the driver's allowlist before the station signs for it. So
//! whatever an agent asks, SOL and dollars only move between Orca's pools
//! and the station's own accounts there.
//!
//! - `open`: the position opened at the station's free slot, then funded,
//!   in one transaction; a buy, an `add` and an `open`'s liquidity are lent
//!   the station's dollars for the call (`fund`), and whatever the call
//!   didn't use goes straight back.
//! - `remove`: liquidity taken out, which brings its fees up to date, then
//!   the fees collected; `collect`: fees brought up to date and collected;
//!   `close`: an emptied position closed, its rent back to the key that paid
//!   it. These are exits: they work on a revoked driver, and lend nothing.
//! - `swap`: SOL bought with the station's dollars or sold for them, no
//!   further than the agent's price.
//!
//! Every send first makes the station's wrapped-SOL account if it isn't
//! there and the tick arrays the call needs if they aren't: both are anyone's
//! to make, and the key pays, billed to the tank like its other sends.
//!
//! The agent hears `changed` after each call that landed, and `price` when a
//! listed pool's price has moved, looked at every few seconds.

use std::time::Duration;

use alloy::primitives::U256;
use anyhow::{Context, Result, anyhow, bail};
use chain::solana::orca::{self as venue, Change, Holdings, PoolNow, Position, TOKEN, asset_is_a};
use chain::solana::programs::anchor_lang::AnchorDeserialize;
use chain::solana::programs::driver::{self as driver_iface, Valuation};
use chain::solana::programs::station::state::{Pinned, Station};
use chain::solana::{Instruction, Pubkey, Signer, SolanaClient};
use hyperliquid::agent::Outcome;
use strategy_sdk::Decimal;
use strategy_sdk::orca::{
    AccountEvent, AccountState, Action, PoolState, PositionState, Receipt, Side,
};
use tokio::sync::mpsc::UnboundedSender;

use super::Trading;
use crate::scale::{to_decimal, to_units};

/// The driver's name in a manifest, and the station's pinned name for it.
pub const DRIVER: &str = strategy_sdk::orca::DRIVER;

/// SOL's decimals and the dollar's.
const SOL_DECIMALS: u32 = venue::ASSET_DECIMALS;
const DOLLAR_DECIMALS: u32 = venue::DOLLAR_DECIMALS;

/// How often the listed pools' prices are looked at for the agent.
const PRICE_EVERY: Duration = Duration::from_secs(5);

/// The station's Orca driver, as the half trades through it.
#[derive(Clone, Debug)]
pub struct Orca {
    /// Its place among the station's drivers.
    pub index: u8,
    pub program: Pubkey,
    /// The station's authority at Orca, and its dollar account.
    pub authority: Pubkey,
    pub dollars: Pubkey,
    /// Where the agent hears of its account.
    pub events: UnboundedSender<Vec<u8>>,
}

impl Orca {
    /// The station's driver pinned as `orca`, which must be the Orca driver
    /// this image lists: a station that pinned something else under that
    /// name, or nothing, isn't traded through here.
    pub fn of(station: &Station, events: UnboundedSender<Vec<u8>>) -> Result<Self> {
        let name = chain::solana::name(DRIVER);
        let (index, pinned) = station
            .drivers
            .iter()
            .enumerate()
            .find(|(_, pinned)| pinned.name == name)
            .ok_or_else(|| anyhow!("the vault's station pins no driver named {DRIVER}"))?;
        Self::checked(index, pinned, events)
    }

    fn checked(index: usize, pinned: &Pinned, events: UnboundedSender<Vec<u8>>) -> Result<Self> {
        if !venue::is_orca(&pinned.program) {
            bail!(
                "the station pinned {} as {DRIVER}, and this image's Orca driver is {}",
                pinned.program,
                chain::solana::programs::orca_driver::ID
            );
        }
        if pinned.venue != venue::WHIRLPOOL_PROGRAM {
            bail!(
                "the station's {DRIVER} driver trades at {}, not Orca",
                pinned.venue
            );
        }
        Ok(Self {
            index: index as u8,
            program: pinned.program,
            authority: pinned.authority,
            dollars: pinned.dollars,
            events,
        })
    }
}

/// Why an action was not sent, or what became of it.
enum Failed {
    /// Refused here, before anything was sent: nothing happened.
    Refused(String),
    /// Sent, and it failed or went unanswered.
    Sent(anyhow::Error),
}

impl From<anyhow::Error> for Failed {
    fn from(error: anyhow::Error) -> Self {
        Failed::Sent(error)
    }
}

fn refused<T>(why: impl Into<String>) -> std::result::Result<T, Failed> {
    Err(Failed::Refused(why.into()))
}

/// A price from an agent, as the chain crate counts prices.
fn price_units(price: Decimal) -> std::result::Result<u128, Failed> {
    let units = to_units(price, venue::PRICE_DECIMALS)
        .map_err(|error| Failed::Refused(format!("{error:#}")))?;
    u128::try_from(units).map_err(|_| Failed::Refused("that price is too large".into()))
}

fn amount_units(amount: Decimal, decimals: u32) -> std::result::Result<u64, Failed> {
    if amount < Decimal::from(0) {
        return refused("an amount can't be negative");
    }
    let units =
        to_units(amount, decimals).map_err(|error| Failed::Refused(format!("{error:#}")))?;
    u64::try_from(units).map_err(|_| Failed::Refused("that amount is too large".into()))
}

fn price_decimal(price: u128) -> Result<Decimal> {
    to_decimal(U256::from(price), venue::PRICE_DECIMALS)
}

fn signed_decimal(value: i128, decimals: u32) -> Result<Decimal> {
    let magnitude = to_decimal(U256::from(value.unsigned_abs()), decimals)?;
    Ok(if value < 0 {
        Decimal::from(0) - magnitude
    } else {
        magnitude
    })
}

/// SOL and dollars, from what the pool keeps as A and B.
fn sol_and_dollars<T>((a, b): (T, T)) -> (T, T) {
    if asset_is_a() { (a, b) } else { (b, a) }
}

/// A and B, from SOL and dollars.
fn a_and_b<T>((sol, dollars): (T, T)) -> (T, T) {
    if asset_is_a() {
        (sol, dollars)
    } else {
        (dollars, sol)
    }
}

/// A position's two prices, the lower first, whichever way the pool keeps
/// its prices.
fn band_of(position: &Position) -> Result<(u128, u128)> {
    let at_lower = venue::price_at_tick(position.tick_lower)?;
    let at_upper = venue::price_at_tick(position.tick_upper)?;
    Ok((at_lower.min(at_upper), at_lower.max(at_upper)))
}

/// Pyth's price as a price here.
fn oracle_units(price: &chain::solana::programs::orca_driver::pyth::Price) -> Option<u128> {
    let exponent = price.exponent + venue::PRICE_DECIMALS as i32;
    let ten = 10u128.checked_pow(exponent.unsigned_abs())?;
    if exponent >= 0 {
        (price.price as u128).checked_mul(ten)
    } else {
        Some(price.price as u128 / ten)
    }
}

/// Whether a pool's price is within `band_bps` of Pyth's, Pyth's price being
/// fit to value with: fully verified, fresh and certain enough.
fn agrees(holdings: &Holdings, pool: &PoolNow, band_bps: u16, now: u64) -> bool {
    let Some(price) = holdings.price else {
        return false;
    };
    if !price.fit(now as i64, venue::MAX_AGE, band_bps) {
        return false;
    }
    let Some(oracle) = oracle_units(&price) else {
        return false;
    };
    let pool_price = venue::price_of_sqrt(pool.state.sqrt_price);
    let gap = pool_price.abs_diff(oracle);
    U256::from(gap) * U256::from(driver_iface::BPS) <= U256::from(oracle) * U256::from(band_bps)
}

/// What the half knows when it translates an action: the station's driver,
/// the key that sends, the station's holdings at Orca, its idle dollars and
/// exit band, and the cluster's clock.
struct Seen<'a> {
    half: &'a Orca,
    key: Pubkey,
    holdings: &'a Holdings,
    idle: u64,
    band_bps: u16,
    now: u64,
}

/// What an action comes to: the venue calls, in order, each with the
/// dollars it is lent; the tick arrays (pool, first tick) they need; and
/// what the agent is answered once they land.
struct Translated {
    calls: Vec<(Instruction, u64)>,
    arrays: Vec<(usize, i32)>,
    answer: Answer,
}

enum Answer {
    Opened {
        position: Pubkey,
        lower_price: u128,
        upper_price: u128,
    },
    /// What moved into the pool or out of it, read from the station.
    Moved,
    Closed {
        position: Pubkey,
    },
}

/// The station calls `action` comes to, checked against what the driver
/// lists and what the station holds; nothing here reads or sends anything.
fn translate(seen: &Seen, action: Action) -> std::result::Result<Translated, Failed> {
    let half = seen.half;
    let holdings = seen.holdings;
    let owners = venue::owners(&half.authority, &half.dollars);
    match action {
        Action::Open {
            pool,
            lower_price,
            upper_price,
            token,
            dollars,
        } => {
            let Some(p) = venue::pool_named(&pool) else {
                return refused(format!("the {DRIVER} driver lists no pool {pool}"));
            };
            let Some(slot) = holdings.free_slot() else {
                return refused(format!(
                    "the station holds {} position(s) at Orca already, the most the driver allows",
                    venue::MAX_SLOTS
                ));
            };
            let pool = &holdings.pools[p];
            if !agrees(holdings, pool, seen.band_bps, seen.now) {
                return refused(
                    "the pool's price is not within the exit band of the oracle's, or the oracle's price is unfit: nothing is opened until it is",
                );
            }
            let ticks = venue::ticks_between(
                price_units(lower_price)?,
                price_units(upper_price)?,
                pool.state.tick_spacing,
            )
            .map_err(|error| Failed::Refused(format!("{error:#}")))?;
            let mint = venue::slot_mint(&half.authority, slot);
            let position = Position {
                whirlpool: pool.address,
                mint,
                liquidity: 0,
                tick_lower: ticks.0,
                tick_upper: ticks.1,
                checkpoint_a: 0,
                fee_owed_a: 0,
                checkpoint_b: 0,
                fee_owed_b: 0,
            };
            let (liquidity, limits, fund) = liquidity(seen, pool, ticks, token, dollars)?;
            let open =
                venue::ix_open_position(&seen.key, &half.authority, &mint, &pool.address, ticks);
            let increase = venue::ix_modify(
                Change::Increase,
                &half.authority,
                pool,
                &position,
                &mint,
                owners,
                liquidity,
                limits,
            );
            let (lower_price, upper_price) =
                band_of(&position).map_err(|error| Failed::Refused(format!("{error:#}")))?;
            let spacing = pool.state.tick_spacing;
            Ok(Translated {
                calls: vec![(open, 0), (increase, fund)],
                arrays: [ticks.0, ticks.1]
                    .map(|tick| (p, venue::array_start(tick, spacing)))
                    .to_vec(),
                answer: Answer::Opened {
                    position: venue::position_address(&mint),
                    lower_price,
                    upper_price,
                },
            })
        }
        Action::Add {
            position,
            token,
            dollars,
        } => {
            let held = held(holdings, &position)?;
            let pool = &holdings.pools[held.pool];
            let ticks = (held.state.tick_lower, held.state.tick_upper);
            let (liquidity, limits, fund) = liquidity(seen, pool, ticks, token, dollars)?;
            let increase = venue::ix_modify(
                Change::Increase,
                &half.authority,
                pool,
                &held.state,
                &held.mint,
                owners,
                liquidity,
                limits,
            );
            Ok(Translated {
                calls: vec![(increase, fund)],
                arrays: Vec::new(),
                answer: Answer::Moved,
            })
        }
        Action::Remove { position, fraction } => {
            let held = held(holdings, &position)?;
            if fraction <= Decimal::from(0) || fraction > Decimal::from(1) {
                return refused("a fraction to remove is above zero and at most one");
            }
            let wad = amount_units(fraction, 18)? as u128;
            let liquidity = (U256::from(held.state.liquidity) * U256::from(wad)
                / U256::from(driver_iface::WAD))
            .to::<u128>();
            let pool = &holdings.pools[held.pool];
            let mut calls = Vec::new();
            // A decrease brings the position's fees up to date itself; with
            // nothing to decrease, an update does.
            if liquidity > 0 {
                let minimums = venue::withdrawal_minimums(
                    pool.state.sqrt_price,
                    (held.state.tick_lower, held.state.tick_upper),
                    liquidity,
                    seen.band_bps,
                )
                .map_err(|error| Failed::Refused(format!("{error:#}")))?;
                calls.push((
                    venue::ix_modify(
                        Change::Decrease,
                        &half.authority,
                        pool,
                        &held.state,
                        &held.mint,
                        owners,
                        liquidity,
                        minimums,
                    ),
                    0,
                ));
            } else if held.state.liquidity > 0 && held.earning {
                calls.push((venue::ix_update_fees(pool, &held.state, &held.mint), 0));
            } else if held.fees_a == 0 && held.fees_b == 0 {
                return refused("that fraction of the position is no liquidity at all");
            }
            calls.push((
                venue::ix_collect_fees(&half.authority, pool, &held.mint, owners),
                0,
            ));
            Ok(Translated {
                calls,
                arrays: Vec::new(),
                answer: Answer::Moved,
            })
        }
        Action::Collect { position } => {
            let held = held(holdings, &position)?;
            let pool = &holdings.pools[held.pool];
            let mut calls = Vec::new();
            if held.state.liquidity > 0 && held.earning {
                calls.push((venue::ix_update_fees(pool, &held.state, &held.mint), 0));
            }
            calls.push((
                venue::ix_collect_fees(&half.authority, pool, &held.mint, owners),
                0,
            ));
            Ok(Translated {
                calls,
                arrays: Vec::new(),
                answer: Answer::Moved,
            })
        }
        Action::Close { position } => {
            let held = held(holdings, &position)?;
            if held.state.liquidity > 0 {
                return refused("the position still holds liquidity: remove it first");
            }
            let pool = &holdings.pools[held.pool];
            let mut calls = Vec::new();
            // Orca closes only a position owing nothing.
            if held.state.fee_owed_a != 0 || held.state.fee_owed_b != 0 {
                calls.push((
                    venue::ix_collect_fees(&half.authority, pool, &held.mint, owners),
                    0,
                ));
            }
            calls.push((
                venue::ix_close_position(&half.authority, &seen.key, &held.mint),
                0,
            ));
            Ok(Translated {
                calls,
                arrays: Vec::new(),
                answer: Answer::Closed {
                    position: held.address,
                },
            })
        }
        Action::Swap {
            pool,
            side,
            amount,
            limit_price,
        } => {
            let Some(p) = venue::pool_named(&pool) else {
                return refused(format!("the {DRIVER} driver lists no pool {pool}"));
            };
            let pool = &holdings.pools[p];
            let buying = matches!(side, Side::Buy);
            let (amount, fund) = if buying {
                let dollars = amount_units(amount, DOLLAR_DECIMALS)?;
                if dollars > seen.idle {
                    return refused(format!(
                        "the station holds {} idle dollars, less than the {} asked",
                        decimal_of(seen.idle, DOLLAR_DECIMALS)?,
                        amount
                    ));
                }
                (dollars, dollars)
            } else {
                let sol = amount_units(amount, SOL_DECIMALS)?;
                if sol > holdings.asset {
                    return refused(format!(
                        "the station holds {} {TOKEN}, less than the {} asked",
                        decimal_of(holdings.asset, SOL_DECIMALS)?,
                        amount
                    ));
                }
                (sol, 0)
            };
            if amount == 0 {
                return refused("a swap of nothing");
            }
            let swap = swap_for(
                &half.authority,
                &half.dollars,
                pool,
                buying,
                amount,
                price_units(limit_price)?,
            )?;
            let a_to_b = buying != asset_is_a();
            Ok(Translated {
                calls: vec![(swap, fund)],
                arrays: venue::swap_starts(&pool.state, a_to_b)
                    .map(|start| (p, start))
                    .to_vec(),
                answer: Answer::Moved,
            })
        }
    }
}

fn decimal_of(units: u64, decimals: u32) -> std::result::Result<Decimal, Failed> {
    to_decimal(U256::from(units), decimals).map_err(|error| Failed::Refused(format!("{error:#}")))
}

/// The liquidity `token` and `dollars` (each capped at what the station
/// holds) pay for between `ticks` at the pool's price now, the most of A and
/// B the pool may take for it, and the dollars lent to the call: the
/// dollars allowed, of which the station takes back what the pool didn't
/// take.
fn liquidity(
    seen: &Seen,
    pool: &PoolNow,
    ticks: (i32, i32),
    token: Decimal,
    dollars: Decimal,
) -> std::result::Result<(u128, (u64, u64), u64), Failed> {
    let sol = amount_units(token, SOL_DECIMALS)?.min(seen.holdings.asset);
    let dollars = amount_units(dollars, DOLLAR_DECIMALS)?.min(seen.idle);
    let (max_a, max_b) = a_and_b((sol, dollars));
    let liquidity = venue::liquidity_for(
        pool.state.tick_current,
        pool.state.sqrt_price,
        ticks,
        max_a,
        max_b,
    )
    .map_err(|error| Failed::Refused(format!("{error:#}")))?;
    if liquidity == 0 {
        return refused(format!(
            "the station's {TOKEN} and dollars asked for buy no liquidity in that band at the pool's price"
        ));
    }
    Ok((liquidity, (max_a, max_b), dollars))
}

impl Trading {
    fn orca(&self) -> Option<&Orca> {
        self.orca.get()
    }

    /// Whether this vault's station trades at Orca through this half.
    pub fn has_orca(&self) -> bool {
        self.orca.get().is_some()
    }

    /// Take the Orca half up for the station's driver pinned as `orca`, its
    /// events going to `events`. Once only; a second call changes nothing.
    pub async fn take_up_orca(&self, events: UnboundedSender<Vec<u8>>) -> Result<()> {
        let station = self
            .platform
            .station(&self.client, &self.accounts.station)
            .await?;
        let half = Orca::of(&station, events)?;
        let _ = self.orca.set(half);
        Ok(())
    }

    /// One action, decoded, checked and sent, and what became of it.
    pub async fn orca_submit(&self, payload: &[u8]) -> Outcome<Vec<u8>> {
        let action: Action = match strategy_sdk::from_payload(payload) {
            Ok(action) => action,
            Err(error) => return Outcome::Refused(format!("that is not an orca action: {error}")),
        };
        let Some(half) = self.orca().cloned() else {
            return Outcome::Refused(format!("this vault has no {DRIVER} account here"));
        };
        match self.orca_act(&half, action).await {
            Ok(receipt) => {
                let _ = half
                    .events
                    .send(strategy_sdk::to_payload(&AccountEvent::Changed));
                Outcome::Accepted(strategy_sdk::to_payload(&receipt))
            }
            Err(Failed::Refused(why)) => Outcome::Refused(why),
            Err(Failed::Sent(error)) => match super::tally_of(&error) {
                crate::host::Tally::Refused => Outcome::Refused(format!("{error:#}")),
                _ => Outcome::Unknown(format!("{error:#}")),
            },
        }
    }

    /// Read what the station holds, translate the action, make what it
    /// needs that isn't there, send it, and say what came of it.
    async fn orca_act(&self, half: &Orca, action: Action) -> std::result::Result<Receipt, Failed> {
        let holdings = Holdings::read(&self.client, &half.authority).await?;
        let station = self
            .platform
            .station(&self.client, &self.accounts.station)
            .await?;
        let seen = Seen {
            half,
            key: self.key.pubkey(),
            holdings: &holdings,
            idle: station.idle,
            band_bps: station.exit_band_bps,
            now: self.client.now().await?,
        };
        let translated = translate(&seen, action)?;
        let mut before = vec![venue::ix_open_wsol(&self.key.pubkey(), &half.authority)];
        before.extend(self.missing_arrays(&holdings, &translated.arrays).await?);
        match translated.answer {
            Answer::Opened {
                position,
                lower_price,
                upper_price,
            } => {
                self.through(half, before, translated.calls).await?;
                Ok(Receipt::Opened {
                    position: position.to_string(),
                    lower_price: price_decimal(lower_price)?,
                    upper_price: price_decimal(upper_price)?,
                })
            }
            Answer::Closed { position } => {
                self.through(half, before, translated.calls).await?;
                Ok(Receipt::Closed {
                    position: position.to_string(),
                })
            }
            Answer::Moved => self.moved(half, before, translated.calls).await,
        }
    }

    /// The tick arrays among `arrays` (pool, first tick) that aren't there
    /// yet, each made by the key: anyone may make one.
    async fn missing_arrays(
        &self,
        holdings: &Holdings,
        arrays: &[(usize, i32)],
    ) -> Result<Vec<Instruction>> {
        let mut wanted: Vec<(Pubkey, i32)> = Vec::new();
        for (p, start) in arrays {
            let pool = &holdings.pools[*p];
            if !venue::array_in_range(*start, pool.state.tick_spacing) {
                continue;
            }
            if !wanted
                .iter()
                .any(|(at, s)| *at == pool.address && s == start)
            {
                wanted.push((pool.address, *start));
            }
        }
        let addresses: Vec<Pubkey> = wanted
            .iter()
            .map(|(pool, start)| venue::tick_array_address(pool, *start))
            .collect();
        let found = self.client.rpc.accounts(&addresses).await?;
        Ok(wanted
            .iter()
            .zip(found)
            .filter(|(_, account)| account.is_none())
            .map(|((pool, start), _)| {
                venue::ix_initialize_tick_array(pool, &self.key.pubkey(), *start)
            })
            .collect())
    }

    /// `before` (the key's own instructions), then each venue call through
    /// the station's guard with what it is lent, in one transaction.
    async fn through(
        &self,
        half: &Orca,
        before: Vec<Instruction>,
        calls: Vec<(Instruction, u64)>,
    ) -> Result<chain::solana::Landed> {
        let mut ixs = before;
        for (call, fund) in calls {
            ixs.push(
                self.platform
                    .ix_execute(
                        &self.client,
                        &self.accounts,
                        &self.bundle,
                        &self.key.pubkey(),
                        half.index,
                        call,
                        fund,
                    )
                    .await?,
            );
        }
        self.send(ixs).await
    }

    /// The same, answered with what moved into the pool (positive) or out
    /// of it (negative): the change in the station's SOL and idle dollars.
    async fn moved(
        &self,
        half: &Orca,
        before: Vec<Instruction>,
        calls: Vec<(Instruction, u64)>,
    ) -> std::result::Result<Receipt, Failed> {
        let wsol = venue::wsol_of(&half.authority);
        let sol_before = self.client.token_balance(&wsol).await.unwrap_or(0) as i128;
        let idle_before = self.idle().await? as i128;
        self.through(half, before, calls).await?;
        let sol_after = self.client.token_balance(&wsol).await.unwrap_or(0) as i128;
        let idle_after = self.idle().await? as i128;
        Ok(Receipt::Moved {
            token: signed_decimal(sol_before - sol_after, SOL_DECIMALS)?,
            dollars: signed_decimal(idle_before - idle_after, DOLLAR_DECIMALS)?,
        })
    }

    async fn idle(&self) -> Result<u64> {
        Ok(self
            .platform
            .station(&self.client, &self.accounts.station)
            .await?
            .idle)
    }

    /// The driver's valuation of the station's holdings at Orca now, at the
    /// station's exit band: the driver itself asked, in a simulation.
    async fn orca_valuation(
        &self,
        half: &Orca,
        holdings: &Holdings,
        band: u16,
    ) -> Result<Valuation> {
        let group = holdings.group(&half.program, &half.dollars);
        let ask = Instruction {
            program_id: half.program,
            accounts: group[1..].to_vec(),
            data: driver_iface::data(driver_iface::discriminator::VALUE, &band),
        };
        let simulated = self.client.simulate(vec![ask], &self.key.pubkey()).await?;
        let (_, data) = simulated
            .return_data
            .ok_or_else(|| anyhow!("the Orca driver answered no valuation"))?;
        Ok(Valuation::try_from_slice(&data)?)
    }

    /// The station's holdings at Orca at the driver's mark, for the balances
    /// an agent reads.
    pub(super) async fn orca_mark(&self) -> Option<u64> {
        let half = self.orca()?;
        let station = self
            .platform
            .station(&self.client, &self.accounts.station)
            .await
            .ok()?;
        let holdings = Holdings::read(&self.client, &half.authority).await.ok()?;
        self.orca_valuation(half, &holdings, station.exit_band_bps)
            .await
            .ok()
            .map(|valuation| valuation.mark)
    }

    /// What `account-state` answers for the Orca driver, as a payload.
    pub async fn orca_state(&self) -> Result<Vec<u8>> {
        let half = self
            .orca()
            .ok_or_else(|| anyhow!("this vault has no {DRIVER} account here"))?;
        let holdings = Holdings::read(&self.client, &half.authority).await?;
        let station = self
            .platform
            .station(&self.client, &self.accounts.station)
            .await?;
        let idle = self.platform.vault(&self.client, &self.vault).await?.cash;
        let valuation = self
            .orca_valuation(half, &holdings, station.exit_band_bps)
            .await
            .context("asking the Orca driver for its valuation")?;
        let state = account_state(&holdings, idle, station.idle, valuation)?;
        Ok(strategy_sdk::to_payload(&state))
    }
}

/// The position an agent names, if it is one the station holds at a slot.
fn held<'a>(
    holdings: &'a Holdings,
    position: &str,
) -> std::result::Result<&'a venue::Held, Failed> {
    let address: Pubkey = match position.parse() {
        Ok(address) => address,
        Err(_) => return refused(format!("{position} is not a position's address")),
    };
    match holdings.position(&address) {
        Some(held) => Ok(held),
        None => refused(format!("the station holds no position {position} at Orca")),
    }
}

/// A swap in `pool` for the station: buying SOL with `amount` dollars or
/// selling `amount` of SOL, no further than `limit` (a price here), which
/// must be on the far side of the pool's price from where the swap starts.
fn swap_for(
    authority: &Pubkey,
    dollars: &Pubkey,
    pool: &PoolNow,
    buying: bool,
    amount: u64,
    limit: u128,
) -> std::result::Result<Instruction, Failed> {
    if limit == 0 {
        return refused("a swap's limit price is above zero");
    }
    // Buying SOL raises its price; whether that raises the pool's own
    // depends on which token it keeps as A.
    let a_to_b = buying != asset_is_a();
    let root = venue::sqrt_of_price(limit);
    let room = if a_to_b {
        root < pool.state.sqrt_price
    } else {
        root > pool.state.sqrt_price
    };
    if !room {
        return refused(if buying {
            "the pool's price is already at or above that limit"
        } else {
            "the pool's price is already at or below that limit"
        });
    }
    Ok(venue::ix_swap(
        authority,
        pool,
        venue::owners(authority, dollars),
        amount,
        0,
        root,
        a_to_b,
    ))
}

/// The agent's picture of the station at Orca.
fn account_state(
    holdings: &Holdings,
    idle: u64,
    station_idle: u64,
    valuation: Valuation,
) -> Result<AccountState> {
    let dollars = |units: u64| to_decimal(U256::from(units), DOLLAR_DECIMALS);
    let sol = |units: u64| to_decimal(U256::from(units), SOL_DECIMALS);
    let mut positions = Vec::new();
    for held in holdings.held() {
        let pool = &holdings.pools[held.pool].state;
        let (token, cash) = sol_and_dollars(venue::holds(&held.state, pool.sqrt_price)?);
        let (fee_token, fee_dollars) = sol_and_dollars((held.fees_a, held.fees_b));
        let (lower, upper) = band_of(&held.state)?;
        positions.push(PositionState {
            position: held.address.to_string(),
            pool: venue::pool_name(held.pool),
            lower_price: price_decimal(lower)?,
            upper_price: price_decimal(upper)?,
            liquidity: held.state.liquidity.to_string(),
            token: sol(token)?,
            dollars: dollars(cash)?,
            fee_token: sol(fee_token)?,
            fee_dollars: dollars(fee_dollars)?,
            in_range: held.state.tick_lower <= pool.tick_current
                && pool.tick_current < held.state.tick_upper,
        });
    }
    let oracle = holdings.price.as_ref();
    let pools = holdings
        .pools
        .iter()
        .enumerate()
        .map(|(p, pool)| {
            Ok(PoolState {
                pool: venue::pool_name(p),
                token: TOKEN.to_string(),
                price: price_decimal(venue::price_of_sqrt(pool.state.sqrt_price))?,
                oracle_price: price_decimal(oracle.and_then(oracle_units).unwrap_or(0))?,
                oracle_time: oracle.map_or(0, |price| price.publish_time.max(0) as u64),
                fee: to_decimal(U256::from(pool.state.fee_rate), 6)?,
                tick_spacing: pool.state.tick_spacing,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let vault_value = idle as u128 + station_idle as u128 + valuation.mark as u128;
    Ok(AccountState {
        vault_value: to_decimal(U256::from(vault_value), DOLLAR_DECIMALS)?,
        idle: dollars(idle)?,
        station_idle: dollars(station_idle)?,
        value: dollars(valuation.mark)?,
        exit_value: dollars(valuation.exit)?,
        tokens: vec![(TOKEN.to_string(), sol(holdings.asset)?)],
        positions,
        pools,
        max_positions: venue::MAX_SLOTS,
    })
}

/// Tell the agent when a listed pool's price moves: every pool looked at
/// every [`PRICE_EVERY`], and a move since the last look said as a `price`
/// event. Ends when the agent is gone.
pub async fn watch_prices(client: SolanaClient, events: UnboundedSender<Vec<u8>>) {
    let mut seen: Vec<Option<u128>> = vec![None; venue::POOLS.len()];
    let addresses: Vec<Pubkey> = venue::POOLS.iter().map(|listed| listed.address).collect();
    let mut unread = crate::Throttle::default();
    loop {
        match client.rpc.accounts(&addresses).await {
            Ok(found) => {
                for (p, (address, account)) in addresses.iter().zip(found).enumerate() {
                    let Some(state) = account
                        .as_ref()
                        .and_then(|account| venue::read_pool(address, account).ok())
                    else {
                        continue;
                    };
                    let moved = seen[p].is_some_and(|before| before != state.sqrt_price);
                    seen[p] = Some(state.sqrt_price);
                    if moved
                        && let Ok(price) = price_decimal(venue::price_of_sqrt(state.sqrt_price))
                    {
                        let event = AccountEvent::Price {
                            pool: venue::pool_name(p),
                            price,
                        };
                        if events.send(strategy_sdk::to_payload(&event)).is_err() {
                            return;
                        }
                    }
                }
            }
            Err(error) => {
                if unread.due(std::time::Instant::now()) {
                    tracing::warn!(
                        error = format!("{error:#}"),
                        "could not read Orca's pools; the agent hears no price until they can be"
                    );
                }
            }
        }
        tokio::time::sleep(PRICE_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chain::solana::AccountMeta;
    use chain::solana::orca::Held;
    use chain::solana::programs::orca_driver::orca::{Whirlpool, ix};
    use chain::solana::programs::orca_driver::pyth::Price;
    use strategy_sdk::decimal;

    const NOW: u64 = 1_800_000_000;
    const SOL: u64 = 1_000_000_000;
    const DOLLAR: u64 = 1_000_000;

    /// Dollars per SOL, as the chain crate counts prices.
    fn price(dollars: u128) -> u128 {
        dollars * 10u128.pow(venue::PRICE_DECIMALS)
    }

    fn half() -> Orca {
        let station = Pubkey::new_unique();
        let authority =
            chain::solana::platform::venue_authority(&station, &venue::WHIRLPOOL_PROGRAM);
        Orca {
            index: 0,
            program: chain::solana::programs::orca_driver::ID,
            authority,
            dollars: Pubkey::new_unique(),
            events: tokio::sync::mpsc::unbounded_channel().0,
        }
    }

    /// The listed pool at `dollars` a SOL, and Pyth agreeing at `oracle`.
    fn holdings(half: &Orca, dollars: u128, oracle: u64, asset: u64) -> Holdings {
        let sqrt_price = venue::sqrt_of_price(price(dollars));
        let (mint_a, mint_b) = venue::network::mints();
        Holdings {
            authority: half.authority,
            pools: vec![PoolNow {
                address: venue::POOLS[0].address,
                state: Whirlpool {
                    config: venue::CONFIG,
                    tick_spacing: venue::POOLS[0].tick_spacing,
                    fee_tier_index: venue::POOLS[0].tick_spacing,
                    fee_rate: 2_000,
                    protocol_fee_rate: 300,
                    liquidity: 1 << 40,
                    sqrt_price,
                    tick_current: venue::tick_at_or_below(sqrt_price),
                    mint_a,
                    vault_a: Pubkey::new_unique(),
                    fee_growth_global_a: 0,
                    mint_b,
                    vault_b: Pubkey::new_unique(),
                    fee_growth_global_b: 0,
                },
            }],
            asset,
            asset_account: true,
            price: Some(Price {
                price: oracle * 100_000_000,
                confidence: 1_000_000,
                exponent: -8,
                publish_time: NOW as i64 - 60,
                full: true,
            }),
            slots: vec![None],
        }
    }

    /// The same, holding a position between $144 and $156 at slot 0.
    fn holding(half: &Orca, liquidity: u128, fees: (u64, u64)) -> (Holdings, Pubkey) {
        let mut holdings = holdings(half, 150, 150, 0);
        let mint = venue::slot_mint(&half.authority, 0);
        let ticks = venue::ticks_between(price(144), price(156), 64).unwrap();
        holdings.slots = vec![Some(Held {
            slot: 0,
            mint,
            address: venue::position_address(&mint),
            pool: 0,
            state: Position {
                whirlpool: venue::POOLS[0].address,
                mint,
                liquidity,
                tick_lower: ticks.0,
                tick_upper: ticks.1,
                checkpoint_a: 0,
                fee_owed_a: fees.0,
                checkpoint_b: 0,
                fee_owed_b: fees.1,
            },
            fees_a: fees.0,
            fees_b: fees.1,
            earning: liquidity > 0,
        })];
        (holdings, venue::position_address(&mint))
    }

    fn seen<'a>(half: &'a Orca, holdings: &'a Holdings, idle: u64) -> Seen<'a> {
        Seen {
            half,
            key: Pubkey::new_unique(),
            holdings,
            idle,
            band_bps: 100,
            now: NOW,
        }
    }

    fn refusal(result: std::result::Result<Translated, Failed>) -> String {
        match result {
            Err(Failed::Refused(why)) => why,
            Err(Failed::Sent(error)) => panic!("sent: {error:#}"),
            Ok(_) => panic!("translated"),
        }
    }

    fn translated(result: std::result::Result<Translated, Failed>) -> Translated {
        match result {
            Ok(translated) => translated,
            Err(Failed::Refused(why)) => panic!("refused: {why}"),
            Err(Failed::Sent(error)) => panic!("sent: {error:#}"),
        }
    }

    fn u64_at(data: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
    }

    fn u128_at(data: &[u8], at: usize) -> u128 {
        u128::from_le_bytes(data[at..at + 16].try_into().unwrap())
    }

    fn open(token: &str, dollars: &str) -> Action {
        Action::Open {
            pool: "SOL/USD".into(),
            lower_price: decimal("144"),
            upper_price: decimal("156"),
            token: decimal(token),
            dollars: decimal(dollars),
        }
    }

    #[test]
    fn an_open_is_the_slot_s_position_opened_then_funded_with_the_dollars_it_may_take() {
        let half = half();
        let holdings = holdings(&half, 150, 150, 2 * SOL);
        let seen = seen(&half, &holdings, 1_000 * DOLLAR);
        let done = translated(translate(&seen, open("1", "150")));
        assert_eq!(done.calls.len(), 2);
        let mint = venue::slot_mint(&half.authority, 0);
        let ticks = venue::ticks_between(price(144), price(156), 64).unwrap();
        // The open: the key pays, the authority owns, the station signs for
        // the slot's mint; nothing is lent.
        let (open, fund) = &done.calls[0];
        assert_eq!(*fund, 0);
        assert_eq!(open.program_id, venue::WHIRLPOOL_PROGRAM);
        assert_eq!(&open.data[..8], &ix::OPEN_POSITION_WITH_TOKEN_EXTENSIONS);
        assert_eq!(open.accounts[0], AccountMeta::new(seen.key, true));
        assert_eq!(
            open.accounts[1],
            AccountMeta::new_readonly(half.authority, false)
        );
        assert_eq!(open.accounts[3], AccountMeta::new(mint, false));
        assert_eq!(
            open.data[8..],
            [&ticks.0.to_le_bytes()[..], &ticks.1.to_le_bytes(), &[0]].concat()
        );
        // The increase: the authority's own accounts, its SOL and dollars up
        // to what was asked, the dollars lent.
        let (increase, fund) = &done.calls[1];
        assert_eq!(&increase.data[..8], &ix::INCREASE_LIQUIDITY);
        assert_eq!(
            increase.accounts[2],
            AccountMeta::new_readonly(half.authority, true)
        );
        let (owner_a, owner_b) = venue::owners(&half.authority, &half.dollars);
        assert_eq!(increase.accounts[5].pubkey, owner_a);
        assert_eq!(increase.accounts[6].pubkey, owner_b);
        assert_eq!(u64_at(&increase.data, 24), SOL);
        assert_eq!(u64_at(&increase.data, 32), 150 * DOLLAR);
        assert_eq!(*fund, 150 * DOLLAR);
        let liquidity = u128_at(&increase.data, 8);
        let pool = &holdings.pools[0].state;
        let (a, b) = venue::deposit(pool.tick_current, pool.sqrt_price, ticks, liquidity).unwrap();
        assert!(a <= SOL && b <= 150 * DOLLAR);
        assert!(a > SOL * 99 / 100 || b > 149 * DOLLAR, "{a} {b}");
        // The arrays its ticks are in, and the answer.
        assert_eq!(
            done.arrays,
            vec![
                (0, venue::array_start(ticks.0, 64)),
                (0, venue::array_start(ticks.1, 64))
            ]
        );
        assert!(matches!(
            done.answer,
            Answer::Opened { position, lower_price, upper_price }
                if position == venue::position_address(&mint)
                    && lower_price <= price(144) && upper_price >= price(156) - 1
        ));
    }

    #[test]
    fn an_open_takes_no_more_than_the_station_holds() {
        let half = half();
        let holdings = holdings(&half, 150, 150, SOL / 2);
        let seen = seen(&half, &holdings, 40 * DOLLAR);
        let done = translated(translate(&seen, open("5", "1000")));
        let (increase, fund) = &done.calls[1];
        assert_eq!(u64_at(&increase.data, 24), SOL / 2);
        assert_eq!(u64_at(&increase.data, 32), 40 * DOLLAR);
        assert_eq!(*fund, 40 * DOLLAR);
    }

    #[test]
    fn an_open_is_refused_off_the_listed_pools_off_the_oracle_or_with_the_slot_taken() {
        let half = half();
        let listed = holdings(&half, 150, 150, SOL);
        let mut elsewhere = open("1", "150");
        if let Action::Open { pool, .. } = &mut elsewhere {
            *pool = "BONK/USD".into();
        }
        assert!(
            refusal(translate(&seen(&half, &listed, 1_000 * DOLLAR), elsewhere))
                .contains("no pool")
        );
        // The pool 2% from the oracle, outside the 1% band.
        let pushed = holdings(&half, 153, 150, SOL);
        assert!(
            refusal(translate(
                &seen(&half, &pushed, 1_000 * DOLLAR),
                open("1", "150")
            ))
            .contains("oracle")
        );
        // A stale price.
        let mut stale = holdings(&half, 150, 150, SOL);
        stale.price.as_mut().unwrap().publish_time = (NOW - venue::MAX_AGE - 1) as i64;
        assert!(
            refusal(translate(
                &seen(&half, &stale, 1_000 * DOLLAR),
                open("1", "150")
            ))
            .contains("oracle")
        );
        let (taken, _) = holding(&half, 1_000, (0, 0));
        assert!(
            refusal(translate(
                &seen(&half, &taken, 1_000 * DOLLAR),
                open("1", "150")
            ))
            .contains("most the driver allows")
        );
        // Nothing to provide.
        assert!(
            refusal(translate(&seen(&half, &listed, 0), open("0", "0"))).contains("no liquidity")
        );
    }

    #[test]
    fn a_buy_lends_the_dollars_it_spends_and_stops_at_the_agent_s_price() {
        let half = half();
        let holdings = holdings(&half, 150, 150, 0);
        let seen = seen(&half, &holdings, 1_000 * DOLLAR);
        let buy = Action::Swap {
            pool: "SOL/USD".into(),
            side: Side::Buy,
            amount: decimal("100"),
            limit_price: decimal("151"),
        };
        let done = translated(translate(&seen, buy));
        let (swap, fund) = &done.calls[0];
        assert_eq!(*fund, 100 * DOLLAR);
        assert_eq!(&swap.data[..8], &ix::SWAP);
        assert_eq!(u64_at(&swap.data, 8), 100 * DOLLAR);
        assert_eq!(u64_at(&swap.data, 16), 0);
        let limit = u128_at(&swap.data, 24);
        assert_eq!(limit, venue::sqrt_of_price(price(151)));
        // The amount is what goes in, and dollars for SOL is B for A.
        assert_eq!(swap.data[40..], [1, u8::from(!asset_is_a())]);
        assert_eq!(
            swap.accounts[1],
            AccountMeta::new_readonly(half.authority, true)
        );
        let starts = venue::swap_starts(&holdings.pools[0].state, !asset_is_a());
        assert_eq!(done.arrays, starts.map(|start| (0, start)).to_vec());
        assert_eq!(
            swap.accounts[7].pubkey,
            venue::tick_array_address(&holdings.pools[0].address, starts[0])
        );
        // More than the station holds, or a limit already passed.
        let too_much = Action::Swap {
            pool: "SOL/USD".into(),
            side: Side::Buy,
            amount: decimal("1000.000001"),
            limit_price: decimal("151"),
        };
        assert!(refusal(translate(&seen, too_much)).contains("idle dollars"));
        let passed = Action::Swap {
            pool: "SOL/USD".into(),
            side: Side::Buy,
            amount: decimal("100"),
            limit_price: decimal("149"),
        };
        assert!(refusal(translate(&seen, passed)).contains("limit"));
    }

    #[test]
    fn a_sale_lends_nothing_and_sells_only_the_station_s_own_sol() {
        let half = half();
        let holdings = holdings(&half, 150, 150, 3 * SOL);
        let seen = seen(&half, &holdings, 0);
        let sell = |amount: &str| Action::Swap {
            pool: "SOL/USD".into(),
            side: Side::Sell,
            amount: decimal(amount),
            limit_price: decimal("149"),
        };
        let done = translated(translate(&seen, sell("2")));
        let (swap, fund) = &done.calls[0];
        assert_eq!(*fund, 0);
        assert_eq!(u64_at(&swap.data, 8), 2 * SOL);
        assert_eq!(swap.data[41], u8::from(asset_is_a()));
        assert!(refusal(translate(&seen, sell("3.000000001"))).contains("SOL"));
    }

    #[test]
    fn a_removal_takes_its_fraction_out_within_the_band_then_collects_and_lends_nothing() {
        let half = half();
        let (holdings, position) = holding(&half, 1_000_000_000, (0, 0));
        let seen = seen(&half, &holdings, 0);
        let remove = |fraction: &str| Action::Remove {
            position: position.to_string(),
            fraction: decimal(fraction),
        };
        let done = translated(translate(&seen, remove("0.5")));
        assert_eq!(done.calls.len(), 2);
        let (decrease, fund) = &done.calls[0];
        assert_eq!(*fund, 0);
        assert_eq!(&decrease.data[..8], &ix::DECREASE_LIQUIDITY);
        assert_eq!(u128_at(&decrease.data, 8), 500_000_000);
        let held = holdings.held().next().unwrap();
        let expected = venue::withdrawal_minimums(
            holdings.pools[0].state.sqrt_price,
            (held.state.tick_lower, held.state.tick_upper),
            500_000_000,
            100,
        )
        .unwrap();
        assert_eq!(
            (u64_at(&decrease.data, 24), u64_at(&decrease.data, 32)),
            expected
        );
        assert_eq!(&done.calls[1].0.data[..8], &ix::COLLECT_FEES);
        assert_eq!(done.calls[1].1, 0);
        for wrong in ["0", "1.5", "-1"] {
            assert!(refusal(translate(&seen, remove(wrong))).contains("fraction"));
        }
        let elsewhere = Action::Remove {
            position: Pubkey::new_unique().to_string(),
            fraction: decimal("1"),
        };
        assert!(refusal(translate(&seen, elsewhere)).contains("no position"));
    }

    #[test]
    fn fees_are_brought_up_to_date_before_they_are_collected() {
        let half = half();
        let (holdings, position) = holding(&half, 1_000_000_000, (7, 9));
        let seen = seen(&half, &holdings, 0);
        let done = translated(translate(
            &seen,
            Action::Collect {
                position: position.to_string(),
            },
        ));
        let names: Vec<[u8; 8]> = done
            .calls
            .iter()
            .map(|(call, _)| call.data[..8].try_into().unwrap())
            .collect();
        assert_eq!(names, vec![ix::UPDATE_FEES_AND_REWARDS, ix::COLLECT_FEES]);
        assert!(done.calls.iter().all(|(_, fund)| *fund == 0));
    }

    #[test]
    fn only_an_emptied_position_is_closed_its_fees_collected_first_and_its_rent_to_the_key() {
        let half = half();
        let (full, position) = holding(&half, 1_000, (0, 0));
        let close = Action::Close {
            position: position.to_string(),
        };
        assert!(refusal(translate(&seen(&half, &full, 0), close.clone())).contains("liquidity"));
        let (owing, _) = holding(&half, 0, (3, 4));
        let seen = seen(&half, &owing, 0);
        let done = translated(translate(&seen, close.clone()));
        let names: Vec<[u8; 8]> = done
            .calls
            .iter()
            .map(|(call, _)| call.data[..8].try_into().unwrap())
            .collect();
        assert_eq!(
            names,
            vec![ix::COLLECT_FEES, ix::CLOSE_POSITION_WITH_TOKEN_EXTENSIONS]
        );
        let (closing, _) = &done.calls[1];
        assert_eq!(
            closing.accounts[0],
            AccountMeta::new_readonly(half.authority, true)
        );
        assert_eq!(closing.accounts[1], AccountMeta::new(seen.key, false));
        assert!(matches!(done.answer, Answer::Closed { position: closed } if closed == position));
        let (empty, _) = holding(&half, 0, (0, 0));
        let done = translated(translate(&self::seen(&half, &empty, 0), close));
        assert_eq!(done.calls.len(), 1);
    }

    #[test]
    fn the_account_state_values_the_vault_as_the_other_halves_do_with_its_band_s_prices() {
        let half = half();
        let (mut holdings, position) = holding(&half, 1_000_000_000_000, (5, 6));
        holdings.asset = 2 * SOL;
        let state = account_state(
            &holdings,
            100 * DOLLAR,
            50 * DOLLAR,
            Valuation {
                mark: 300 * DOLLAR,
                exit: 299 * DOLLAR,
                outside: false,
            },
        )
        .unwrap();
        assert_eq!(state.vault_value, decimal("450"));
        assert_eq!(state.idle, decimal("100"));
        assert_eq!(state.station_idle, decimal("50"));
        assert_eq!(state.value, decimal("300"));
        assert_eq!(state.exit_value, decimal("299"));
        assert_eq!(state.token("SOL"), decimal("2"));
        assert_eq!(state.max_positions, 1);
        let held = &state.positions[0];
        assert_eq!(held.position, position.to_string());
        assert_eq!(held.pool, "SOL/USD");
        assert!(held.lower_price < decimal("144.0001") && held.lower_price > decimal("143"));
        assert!(held.upper_price > decimal("155.999") && held.upper_price < decimal("157"));
        assert!(held.in_range);
        assert_eq!(held.liquidity, "1000000000000");
        assert_eq!(
            (held.fee_token, held.fee_dollars),
            (decimal("0.000000005"), decimal("0.000006"))
        );
        let pool = state.pool("SOL/USD").unwrap();
        assert_eq!(pool.oracle_price, decimal("150"));
        assert_eq!(pool.fee, decimal("0.002"));
        assert_eq!(pool.tick_spacing, 64);
        assert!(pool.price > decimal("149.99") && pool.price < decimal("150.01"));
    }
}
