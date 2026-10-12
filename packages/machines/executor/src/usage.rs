//! What running a vault's agent has cost: one running total, signed with the
//! vault's key and handed to whoever collects it.
//!
//! The total is the author's bill and nobody else's: a tank is the author's
//! balance, never depositors' money. The vault believes it only up to the
//! allowances the author set, at the published prices, for the time that has
//! passed, so a wrong total can never charge more than the author agreed to.
//!
//! What it costs is the capacity the agent held, for the time it held it: the
//! allowances in force, whether or not it spent them, for the time the agent
//! was running. An agent stopped for a dry tank holds nothing, so nothing is
//! billed until a top-up starts it again. The time is measured on the wall
//! clock, not the chain's, because a development chain's clock runs fast and
//! a machine costs what it costs however quickly the chain believes the days
//! are passing.
//!
//! Gas is the exception: it is billed as spent, from the receipts of the
//! agent's own transactions, at the native coin's price, and never beyond the
//! gas allowance accrued so far. Allowance that went unspent carries forward,
//! the way the vault's own cap does, and like it never more than a day's
//! worth: a burst inside a day is billed, a month left idle isn't saved up
//! to be spent at once. A chain with no price for its coin bills
//! no gas at all: an author never pays for gas nobody priced.
//!
//! The total is cumulative and never goes back: it starts from what the vault
//! has already collected, and if the vault collects a larger total signed by
//! an instance before this one, this one carries on from that. A collected
//! figure is carried on from only once two reads of it agree, each at a
//! block the chain's confirmations deep and the second at a later block
//! (three blocks later on a chain read at its head), or, on a chain whose
//! head stands still, a minute apart: one wrong answer from a node is never
//! carried in every later total. What it was collected under (allowances
//! since lowered, another executor's higher ceilings) is not second-guessed
//! here: the vault took it, and never takes more than its own cap allows.
//!
//! When the unpaid part of the total reaches what the tank holds, the agent is
//! stopped, and it starts again on a top-up: the contract has no "dry" state,
//! and stopping is this executor's job.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, Bytes, U256};
use anyhow::{Context, Result};
use chain::bindings::vault::IVault;
use chain::{Client, KeyClient};
use serde::Serialize;
use shared::{Allowances, Prices};

use crate::ceilings::Ceilings;
use crate::scale::ASSET_DECIMALS;

/// A signed running total, as the keeper collects it.
#[derive(Clone, Debug, Serialize)]
pub struct Signed {
    /// In the asset's smallest unit.
    pub total: String,
    pub as_of: u64,
    pub signature: Bytes,
}

/// The latest total for each vault this executor serves, by chain id and
/// vault.
pub type Totals = Arc<Mutex<HashMap<(u64, Address), Signed>>>;

/// What the meter says about the tank, for the agent and its reports.
#[derive(Clone, Default)]
pub struct Tank {
    /// Whether the unpaid part of the total has reached the tank's balance.
    pub dry: Arc<AtomicBool>,
    /// Whether the meter has taken a reading at all: until it has, `dry`
    /// means nothing yet.
    pub metered: Arc<AtomicBool>,
    /// The moment the latest signed total counts up to.
    pub signed_through: SignedThrough,
}

/// The moment the meter's latest signed total counts up to: everything the
/// agent ran before it is in that total.
#[derive(Clone, Default)]
pub struct SignedThrough(Arc<Mutex<Option<Instant>>>);

impl SignedThrough {
    pub(crate) fn signed(&self, through: Instant) {
        if let Ok(mut at) = self.0.lock() {
            *at = Some(through);
        }
    }

    /// As if a total had just been signed, counting up to now.
    #[cfg(test)]
    pub fn signed_now(&self) {
        self.signed(Instant::now());
    }

    /// Whether a signed total counts everything up to `moment`.
    pub fn covers(&self, moment: Instant) -> bool {
        self.0
            .lock()
            .ok()
            .and_then(|at| *at)
            .is_some_and(|at| at >= moment)
    }
}

/// How often the running flag is looked at, and how many looks make a tick:
/// a total is signed once a tick.
pub(crate) const SAMPLE: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(1)
};
pub(crate) const SAMPLES_PER_TICK: u32 = 5;

/// Meter one vault, keep its signed total current, and say when its tank is
/// dry. `running` is whether the agent is running at this moment: capacity is
/// billed only for the time it is, and only at what the agent is given, each
/// allowance held to its ceiling.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    chain: Client,
    key: KeyClient,
    vault: Address,
    native_usd: Option<f64>,
    totals: Totals,
    tank: Tank,
    running: Arc<AtomicBool>,
    ceilings: Ceilings,
    confirmations: u64,
) {
    let Tank {
        dry,
        metered,
        signed_through,
    } = tank;
    if native_usd.is_none() {
        tracing::warn!(%vault, "this chain's coin has no price: the gas the agent spends is not billed");
    }
    // Nothing is counted for the time before this instance took the vault
    // on, nor gas spent before it. A vault taken on again carries on from the
    // total this process last signed for it, whatever the vault has collected.
    let signed_before = totals
        .lock()
        .ok()
        .and_then(|totals| {
            totals
                .get(&(chain.chain_id, vault))
                .and_then(|signed| signed.total.parse::<U256>().ok())
        })
        .unwrap_or_default();
    let mut meter = Meter::new(native_usd, key.gas_spent()).carrying_on(signed_before);
    let mut elapsed = Elapsed::default();
    // The allowances found at the last reading, at which the time since it
    // is billed.
    let mut in_force: Option<Allowances> = None;
    let mut since = Instant::now();
    let mut unread = Unread::new(Instant::now());
    loop {
        for _ in 0..SAMPLES_PER_TICK {
            tokio::time::sleep(SAMPLE).await;
            let now = Instant::now();
            let seconds = now.duration_since(since).as_secs_f64();
            since = now;
            elapsed.seconds += seconds;
            if running.load(Ordering::SeqCst) {
                elapsed.running += seconds;
            }
        }
        match tick(
            &chain,
            &key,
            vault,
            &mut meter,
            &mut elapsed,
            &mut in_force,
            &ceilings,
            confirmations,
        )
        .await
        {
            Ok((signed, is_dry)) => {
                if let Ok(mut totals) = totals.lock() {
                    totals.insert((chain.chain_id, vault), signed);
                }
                // The tick counted the time up to its last look at the
                // running flag.
                signed_through.signed(since);
                if dry.swap(is_dry, Ordering::SeqCst) != is_dry {
                    if is_dry {
                        tracing::warn!(%vault, "the tank is dry: the agent stops until it is topped up");
                    } else {
                        tracing::info!(%vault, "the tank has money again");
                    }
                }
                metered.store(true, Ordering::SeqCst);
                unread.read(Instant::now());
            }
            Err(error) => {
                let held = !metered.load(Ordering::SeqCst);
                match unread.failed(Instant::now()) {
                    Some(for_how_long) => tracing::warn!(
                        %vault,
                        seconds = for_how_long.as_secs(),
                        held,
                        error = format!("{error:#}"),
                        "the vault has not been metered for a while; until it is, nothing is billed and an agent not yet started is held back"
                    ),
                    None => {
                        tracing::info!(%vault, error = format!("{error:#}"), "could not meter the vault")
                    }
                }
            }
        }
    }
}

/// How long a meter may go without a reading before that is worth a warning,
/// and how often the warning is said again while it lasts.
const UNREAD_WARN_AFTER: Duration = Duration::from_secs(30);
const UNREAD_WARN_EVERY: Duration = Duration::from_secs(60);

/// When the meter last read the vault, and when it last warned that it
/// couldn't.
struct Unread {
    since: Instant,
    warned: Option<Instant>,
}

impl Unread {
    fn new(now: Instant) -> Self {
        Self {
            since: now,
            warned: None,
        }
    }

    fn read(&mut self, now: Instant) {
        self.since = now;
        self.warned = None;
    }

    /// A reading failed. How long the meter has gone without one, if that is
    /// long enough to warn of and no warning was given in the last minute.
    fn failed(&mut self, now: Instant) -> Option<Duration> {
        let unread = now.duration_since(self.since);
        if unread < UNREAD_WARN_AFTER
            || self
                .warned
                .is_some_and(|at| now.duration_since(at) < UNREAD_WARN_EVERY)
        {
            return None;
        }
        self.warned = Some(now);
        Some(unread)
    }
}

/// The wall-clock time since the meter last counted, and how much of it the
/// agent was running for.
#[derive(Default)]
struct Elapsed {
    seconds: f64,
    running: f64,
}

/// What one vault has run up since this instance took it on, in the asset's
/// smallest unit, and the total it carries on from.
pub(crate) struct Meter {
    /// What the total counts on from: what the vault had collected when the
    /// meter started, or the total an earlier meter in this process signed
    /// for the vault, or a larger total another instance signed and the vault
    /// collected since.
    base: U256,
    /// The largest total this meter has signed.
    signed: U256,
    /// A collected figure above `signed` not yet carried on from, the block
    /// it was first read at, and when: it is carried on from once a later
    /// read finds it again (see [`Meter::agreed`]), every read between
    /// agreeing.
    unconfirmed: Option<(U256, u64, Instant)>,
    native_usd: Option<f64>,
    /// The capacity held while the agent ran, gas aside.
    capacity: f64,
    /// The gas allowance accrued so far: the most gas can be billed. What
    /// goes unspent is carried, up to a day's allowance.
    gas_allowed: f64,
    /// The gas actually paid for, priced as it was spent.
    gas_spent: f64,
    /// The key's gas counter when it was last read, in the native unit.
    gas_seen: u128,
}

impl Meter {
    pub(crate) fn new(native_usd: Option<f64>, gas_seen: u128) -> Self {
        Self {
            base: U256::ZERO,
            signed: U256::ZERO,
            unconfirmed: None,
            native_usd,
            capacity: 0.0,
            gas_allowed: 0.0,
            gas_spent: 0.0,
            gas_seen,
        }
    }

    /// Carry on from a total an earlier meter for the vault signed, so a
    /// vault let go and taken on again never serves a smaller total.
    pub(crate) fn carrying_on(mut self, signed: U256) -> Self {
        self.base = signed;
        self.signed = signed;
        self
    }

    /// Add `seconds` at these allowances, `running` of which the agent was
    /// running for, and whatever gas the key has paid since the last reading.
    pub(crate) fn add(
        &mut self,
        seconds: f64,
        running: f64,
        allowances: &Allowances,
        gas_now: u128,
    ) {
        let unit = 10f64.powi(ASSET_DECIMALS as i32);
        self.capacity += cost(running.min(seconds), allowances);
        let day = allowances.gas_per_day * unit;
        self.gas_allowed += day * (seconds / 86_400.0);
        // A rise is what the key paid; a fall is what it got back (rent an
        // account closed returned to it), credited against what was billed,
        // never below nothing: the same lamports paid, refunded and paid
        // again are billed once, and nothing never billed is credited.
        let (paid, back) = (
            gas_now.saturating_sub(self.gas_seen),
            self.gas_seen.saturating_sub(gas_now),
        );
        self.gas_seen = gas_now;
        if let Some(price) = self.native_usd {
            self.gas_spent += paid as f64 / 1e18 * price * unit;
            self.gas_spent = (self.gas_spent - back as f64 / 1e18 * price * unit).max(0.0);
        }
        // Unspent allowance is carried, but no more than a day's: the vault
        // carries its own unused cap the same way, and never charges more.
        self.gas_allowed = self.gas_allowed.min(self.gas_spent + day);
    }

    /// The gas billed: what was spent, never beyond what was allowed. With
    /// no price for the coin, none.
    fn gas(&self) -> f64 {
        match self.native_usd {
            Some(_) => self.gas_spent.min(self.gas_allowed),
            None => 0.0,
        }
    }

    fn accrued(&self) -> U256 {
        U256::from((self.capacity + self.gas()).round().max(0.0) as u128)
    }

    /// Whether the vault's collected figure, read as of block `block` at
    /// `now`, is to be carried on from. A collected total above anything
    /// this meter signed was another instance's, and this one's accrual is
    /// counted on top of it: a total below what the vault has collected
    /// would be refused as stale. But a node can answer one read wrongly, so
    /// the figure is believed only once a read at a block at least `apart`
    /// later finds the same, every read between agreeing; or, should the
    /// head stand still, a read [`AGREE_AFTER`] later. Each read is taken
    /// the chain's confirmations deep, so a collection a reorg undoes is not
    /// carried on from either.
    ///
    /// The figure is not held to what this executor thinks the vault could
    /// have run up: it may have been collected under allowances since
    /// lowered, or from an executor run with higher ceilings, and a meter
    /// signing below it would have every collection refused as stale, the
    /// agent never billed and never stopped. The vault's own cap bounds what
    /// any total, however it was reached, can take from the tank.
    pub(crate) fn agreed(&mut self, settled: U256, block: u64, apart: u64, now: Instant) -> bool {
        if settled <= self.signed {
            self.unconfirmed = None;
            return false;
        }
        match self.unconfirmed {
            Some((figure, first, at)) if figure == settled => {
                block >= first.saturating_add(apart.max(1))
                    || now.saturating_duration_since(at) >= AGREE_AFTER
            }
            _ => {
                self.unconfirmed = Some((settled, block, now));
                false
            }
        }
    }

    /// Carry on from an agreed collected figure.
    pub(crate) fn carry_on_from(&mut self, settled: U256) {
        self.unconfirmed = None;
        self.base = self.base.max(settled);
    }

    /// The total to sign: what it carries on from, and what has accrued.
    pub(crate) fn total(&mut self) -> U256 {
        let total = self.base + self.accrued();
        self.signed = self.signed.max(total);
        total
    }
}

/// How many blocks apart two reads of the vault's collected figure must be,
/// agreeing, before the meter carries on from it, on a chain read at its
/// head (no confirmations). Read deeper, a later block is enough.
pub(crate) const AGREE_BLOCKS: u64 = 3;

/// How far apart two agreeing reads of the collected figure may be in time,
/// rather than in blocks, for a chain whose head doesn't move.
const AGREE_AFTER: Duration = Duration::from_secs(60);

/// How many blocks apart two reads of the collected figure, each taken
/// `confirmations` deep, must be to agree.
fn agree_blocks(confirmations: u64) -> u64 {
    if confirmations == 0 { AGREE_BLOCKS } else { 1 }
}

/// Take one reading, bill the time since the last good one, and sign the
/// total. The time since the last good reading is billed at the allowances
/// that reading found (`in_force`), which the agent ran under: a change the
/// author makes is billed from the reading that first finds it, and a
/// stretch of failed readings is never billed at what is found after it.
/// The agent is started again under new allowances once it sees them, so
/// this is at most a reading late either way.
#[allow(clippy::too_many_arguments)]
async fn tick(
    chain: &Client,
    key: &KeyClient,
    vault: Address,
    meter: &mut Meter,
    elapsed: &mut Elapsed,
    in_force: &mut Option<Allowances>,
    ceilings: &Ceilings,
    confirmations: u64,
) -> Result<(Signed, bool)> {
    // The head before the read: the read is of this block or a later one.
    let head = chain.block_number().await?;
    let tank = chain.read(vault, IVault::tankCall {}).await?;
    // What the vault had collected as of a block the chain's confirmations
    // deep, for carrying on from: at the head, the read already taken.
    let (settled, settled_at) = if confirmations == 0 {
        (tank.usageSettled, head)
    } else if let Some(deep) = head.checked_sub(confirmations) {
        let at = alloy::eips::BlockId::number(deep);
        let deep_tank = chain.read_at(vault, IVault::tankCall {}, at).await?;
        (deep_tank.usageSettled, deep)
    } else {
        // A chain younger than its confirmations has nothing that deep.
        (U256::ZERO, 0)
    };
    let found = ceilings.hold(&shared_allowances(
        &chain.read(vault, IVault::allowancesCall {}).await?,
    ));
    meter.add(
        elapsed.seconds,
        elapsed.running,
        in_force.as_ref().unwrap_or(&found),
        key.gas_spent(),
    );
    *elapsed = Elapsed::default();
    *in_force = Some(found);
    let as_of = chain.block_timestamp().await?;
    if meter.agreed(
        settled,
        settled_at,
        agree_blocks(confirmations),
        Instant::now(),
    ) {
        meter.carry_on_from(settled);
    }
    let total = meter.total();

    // Worked out here, never read from the node: whoever answers the RPC
    // could hand over the digest of any total, or of anything else this key's
    // signature counts for, and have it signed. The chain id is the
    // configured one, checked against the node when it connected.
    let digest = chain::digest::usage(chain.chain_id, vault, total, as_of);
    let signature = key.sign(digest).context("signing the total")?;

    let unpaid = total.saturating_sub(tank.usageSettled);
    Ok((
        Signed {
            total: total.to_string(),
            as_of,
            signature,
        },
        unpaid >= tank.balance,
    ))
}

/// What holding these allowances for this long costs at the published prices,
/// in the asset's smallest unit, gas aside: gas is billed as it is spent.
fn cost(seconds: f64, allowances: &Allowances) -> f64 {
    let held = Allowances {
        gas_per_day: 0.0,
        ..*allowances
    };
    let day = Prices::published().daily_cap(&held);
    day * (seconds / 86_400.0) * 10f64.powi(ASSET_DECIMALS as i32)
}

/// The vault's allowances as the platform's own type.
pub fn shared_allowances(on_chain: &chain::bindings::prices::Allowances) -> Allowances {
    Allowances {
        fuel_per_sec: on_chain.fuelPerSec,
        memory_mib: on_chain.memoryMib,
        requests_per_min: on_chain.requestsPerMin,
        bytes_per_day: on_chain.bytesPerDay,
        streams: on_chain.streams,
        inference_ms_per_min: on_chain.inferenceMsPerMin,
        gas_per_day: on_chain.gasPerDay as f64 / 1_000_000.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::stand_in;

    const DAY: f64 = 86_400.0;
    const ETHER: u128 = 1_000_000_000_000_000_000;

    fn meter(native_usd: Option<f64>) -> Meter {
        Meter::new(native_usd, 0)
    }

    /// The total a tick signs having read `settled` at the head, `head`, of
    /// a chain read with no confirmations, all in the same moment.
    fn read(meter: &mut Meter, settled: u64, head: u64) -> U256 {
        read_when(meter, settled, head, AGREE_BLOCKS, Instant::now())
    }

    /// The total a tick signs having read `settled` as of `block` at `now`,
    /// two reads agreeing `apart` blocks apart.
    fn read_when(meter: &mut Meter, settled: u64, block: u64, apart: u64, now: Instant) -> U256 {
        let settled = U256::from(settled);
        if meter.agreed(settled, block, apart, now) {
            meter.carry_on_from(settled);
        }
        meter.total()
    }

    fn gas_only(per_day: f64) -> Allowances {
        Allowances {
            fuel_per_sec: 0,
            memory_mib: 0,
            requests_per_min: 0,
            bytes_per_day: 0,
            streams: 0,
            inference_ms_per_min: 0,
            gas_per_day: per_day,
        }
    }

    fn modest_capacity_for(seconds: f64) -> U256 {
        let modest = Allowances::modest();
        let day = Prices::published().daily_cap(&Allowances {
            gas_per_day: 0.0,
            ..modest
        });
        U256::from((day * seconds / DAY * 1e6).round() as u128)
    }

    #[test]
    fn a_meter_that_cannot_read_the_vault_warns_after_half_a_minute_and_then_once_a_minute() {
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let mut unread = Unread::new(start);
        assert_eq!(unread.failed(at(5)), None);
        assert_eq!(unread.failed(at(29)), None);
        assert_eq!(unread.failed(at(30)), Some(Duration::from_secs(30)));
        assert_eq!(unread.failed(at(35)), None);
        assert_eq!(unread.failed(at(89)), None);
        assert_eq!(unread.failed(at(90)), Some(Duration::from_secs(90)));
        // A reading starts the count again.
        unread.read(at(100));
        assert_eq!(unread.failed(at(110)), None);
        assert_eq!(unread.failed(at(130)), Some(Duration::from_secs(30)));
    }

    #[test]
    fn gas_is_billed_as_spent() {
        // A dollar a day allowed, a thousandth of a coin at $2,000 spent: $2
        // allowed would cap it, but only a day has passed, so $1 is billed.
        let mut meter = meter(Some(2_000.0));
        meter.add(DAY, DAY, &gas_only(1.0), ETHER / 1_000);
        assert_eq!(meter.accrued(), U256::from(1_000_000u64));

        // A quiet second day: the unspent allowance carries, and the rest of
        // the gas already spent is billed.
        meter.add(DAY, DAY, &gas_only(1.0), ETHER / 1_000);
        assert_eq!(meter.accrued(), U256::from(2_000_000u64));
    }

    #[test]
    fn unspent_gas_allowance_is_carried_for_a_day_at_most() {
        // A month at a dollar a day with nothing spent, then $5 of gas at
        // once: only the day's dollar carried and the moment's own share is
        // allowed for it.
        let mut idle = meter(Some(2_000.0));
        idle.add(30.0 * DAY, 30.0 * DAY, &gas_only(1.0), 0);
        idle.add(1.0, 1.0, &gas_only(1.0), 5 * ETHER / 2_000);
        let billed = idle.accrued().to::<u64>();
        assert!(
            (1_000_000..=1_000_100).contains(&billed),
            "billed {billed}: a month of unspent allowance was saved up"
        );

        // Inside a day, a burst is billed in full against what the day left.
        let mut busy = meter(Some(2_000.0));
        busy.add(DAY / 2.0, DAY / 2.0, &gas_only(2.0), 0);
        busy.add(1.0, 1.0, &gas_only(2.0), ETHER / 2_000);
        assert_eq!(busy.accrued(), U256::from(1_000_000u64));
    }

    #[test]
    fn unspent_gas_is_not_billed() {
        let mut meter = meter(Some(2_000.0));
        meter.add(DAY, DAY, &gas_only(5.0), ETHER / 10_000);
        // A tenth of a thousandth of a coin at $2,000 is twenty cents.
        assert_eq!(meter.accrued(), U256::from(200_000u64));
    }

    #[test]
    fn a_meter_carrying_on_never_signs_less_than_the_last() {
        let mut meter = meter(Some(2_000.0)).carrying_on(U256::from(5_000u64));
        meter.add(DAY, DAY, &Allowances::modest(), 0);
        let total = read(&mut meter, 1_000, 16);
        assert_eq!(total, U256::from(5_000u64) + modest_capacity_for(DAY));
    }

    #[test]
    fn gas_spent_before_the_meter_started_is_not_billed() {
        let mut meter = Meter::new(Some(2_000.0), ETHER);
        meter.add(DAY, DAY, &gas_only(5.0), ETHER);
        assert_eq!(meter.accrued(), U256::ZERO);
    }

    #[test]
    fn an_unpriced_coin_bills_no_gas() {
        let mut meter = meter(None);
        meter.add(DAY / 2.0, DAY / 2.0, &gas_only(3.0), ETHER);
        assert_eq!(meter.accrued(), U256::ZERO);
    }

    #[test]
    fn capacity_is_billed_whether_or_not_it_is_used() {
        let mut meter = meter(Some(2_000.0));
        meter.add(DAY, DAY, &Allowances::modest(), 0);
        assert_eq!(meter.accrued(), modest_capacity_for(DAY));
    }

    #[test]
    fn capacity_is_billed_only_while_the_agent_runs() {
        let mut meter = meter(Some(2_000.0));
        // A day, of which the agent ran for a quarter.
        meter.add(DAY, DAY / 4.0, &Allowances::modest(), 0);
        assert_eq!(meter.accrued(), modest_capacity_for(DAY / 4.0));
        // Stopped all the next day: nothing more.
        meter.add(DAY, 0.0, &Allowances::modest(), 0);
        assert_eq!(meter.accrued(), modest_capacity_for(DAY / 4.0));
    }

    #[test]
    fn a_meter_carries_on_from_what_the_vault_has_collected_once_two_reads_agree() {
        let mut meter = meter(Some(2_000.0));
        // What the vault had collected is believed once a read three blocks
        // on finds it again.
        assert_eq!(read(&mut meter, 500, 16), U256::ZERO);
        assert_eq!(read(&mut meter, 500, 18), U256::ZERO);
        assert_eq!(read(&mut meter, 500, 19), U256::from(500u64));
        meter.add(DAY, 0.0, &gas_only(1.0), ETHER / 1_000_000);
        // A two-thousandth of a dollar of gas.
        assert_eq!(read(&mut meter, 500, 20), U256::from(2_500u64));
        // Its own total collected changes nothing.
        assert_eq!(read(&mut meter, 2_500, 21), U256::from(2_500u64));
        // A larger total from an instance before it was collected: once it
        // is read again blocks later, this one's accrual is counted on top.
        assert_eq!(read(&mut meter, 9_000, 22), U256::from(2_500u64));
        assert_eq!(read(&mut meter, 9_000, 25), U256::from(11_000u64));
        assert_eq!(read(&mut meter, 9_000, 26), U256::from(11_000u64));
    }

    #[test]
    fn one_wrong_read_of_the_collected_figure_is_never_carried_on_from() {
        let mut meter = meter(Some(2_000.0));
        meter.add(DAY, 0.0, &gas_only(1.0), ETHER / 1_000_000);
        // A node answers one read with a figure nobody collected, then the
        // truth, then the same figure again blocks later.
        assert_eq!(read(&mut meter, 1_000_000, 16), U256::from(2_000u64));
        assert_eq!(read(&mut meter, 0, 17), U256::from(2_000u64));
        assert_eq!(read(&mut meter, 1_000_000, 20), U256::from(2_000u64));
        // Read twice in the same few blocks, it is still not believed.
        assert_eq!(read(&mut meter, 1_000_000, 21), U256::from(2_000u64));
        assert_eq!(read(&mut meter, 0, 30), U256::from(2_000u64));
    }

    #[test]
    fn a_figure_read_confirmations_deep_is_carried_on_from_once_a_later_block_agrees() {
        let now = Instant::now();
        let mut meter = meter(Some(2_000.0));
        // Read deep, the same block twice is one read, not two.
        assert_eq!(read_when(&mut meter, 700, 4, 1, now), U256::ZERO);
        assert_eq!(read_when(&mut meter, 700, 4, 1, now), U256::ZERO);
        // The next block agrees.
        assert_eq!(read_when(&mut meter, 700, 5, 1, now), U256::from(700u64));
    }

    #[test]
    fn on_a_chain_whose_head_stands_still_two_reads_a_minute_apart_agree() {
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let mut meter = meter(Some(2_000.0));
        assert_eq!(
            read_when(&mut meter, 900, 16, AGREE_BLOCKS, at(0)),
            U256::ZERO
        );
        assert_eq!(
            read_when(&mut meter, 900, 16, AGREE_BLOCKS, at(59)),
            U256::ZERO
        );
        assert_eq!(
            read_when(&mut meter, 900, 16, AGREE_BLOCKS, at(60)),
            U256::from(900u64)
        );
        // A read between that disagrees starts the minute again.
        let mut meter = Meter::new(Some(2_000.0), 0);
        assert_eq!(
            read_when(&mut meter, 900, 16, AGREE_BLOCKS, at(0)),
            U256::ZERO
        );
        assert_eq!(
            read_when(&mut meter, 0, 16, AGREE_BLOCKS, at(30)),
            U256::ZERO
        );
        assert_eq!(
            read_when(&mut meter, 900, 16, AGREE_BLOCKS, at(61)),
            U256::ZERO
        );
        assert_eq!(
            read_when(&mut meter, 900, 16, AGREE_BLOCKS, at(121)),
            U256::from(900u64)
        );
    }

    /// Allowances that accrue over ten units a second, held in full by the
    /// default ceilings, so a test of totals doesn't hang on what a preset
    /// happens to cost.
    fn accruing() -> Allowances {
        Allowances {
            fuel_per_sec: Ceilings::FUEL_PER_SEC,
            memory_mib: Ceilings::MEMORY_MIB,
            bytes_per_day: 5_000_000_000,
            ..Allowances::modest()
        }
    }

    #[test]
    fn the_accruing_allowances_accrue_over_ten_units_a_second_and_are_held_in_full() {
        assert_eq!(Ceilings::default().hold(&accruing()), accruing());
        assert!(
            per_second(&accruing()) > 10.0,
            "{}",
            per_second(&accruing())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dry_tank_stops_the_bill_until_it_is_topped_up() {
        // A tank with a millionth of a dollar in it: the first tick runs it
        // dry.
        let state = stand_in::chain(1, 0);
        state.lock().unwrap().allowances = chain::allowances(&accruing());
        let client = stand_in::client(&stand_in::start(state.clone()).await).await;
        let key = stand_in::key(&client);
        let vault = Address::repeat_byte(0x11);
        let totals: Totals = Default::default();
        let dry = Arc::new(AtomicBool::new(false));
        let metered = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(true));
        let meter = tokio::spawn(run(
            client,
            key,
            vault,
            Some(2_500.0),
            totals.clone(),
            Tank {
                dry: dry.clone(),
                metered: metered.clone(),
                signed_through: Default::default(),
            },
            running.clone(),
            Ceilings::default(),
            0,
        ));
        assert!(
            !metered.load(Ordering::SeqCst),
            "nothing is known of the tank before the first reading"
        );

        let at_stop = stand_in::signed_total(&totals, vault, None).await;
        assert!(dry.load(Ordering::SeqCst), "the first tick runs it dry");
        assert!(
            metered.load(Ordering::SeqCst),
            "the first reading was taken"
        );
        assert!(at_stop > 1, "the first tick accrued something: {at_stop}");
        // The agent is stopped for the dry tank, as the executor stops it.
        running.store(false, Ordering::SeqCst);

        // Three more ticks with the agent stopped: the bill stands still.
        tokio::time::sleep(SAMPLE * SAMPLES_PER_TICK * 3).await;
        assert_eq!(stand_in::latest_total(&totals, vault), Some(at_stop));

        // A top-up, and the tank is no longer dry.
        state.lock().unwrap().tank.balance = U256::from(1_000_000_000u64);
        tokio::time::sleep(SAMPLE * SAMPLES_PER_TICK * 2).await;
        assert!(!dry.load(Ordering::SeqCst), "the top-up starts it again");
        meter.abort();
    }

    /// What these allowances cost a second, gas aside, in the asset's units.
    fn per_second(allowances: &Allowances) -> f64 {
        cost(1.0, allowances)
    }

    /// The node is down for three seconds while the agent runs at cheap
    /// allowances; the author raises them a hundredfold and the node comes
    /// back. The outage is billed at the allowances the agent ran under, not
    /// at those found after it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_streak_of_failed_meter_readings_is_billed_at_the_allowances_found_before_it() {
        let cheap = Allowances {
            gas_per_day: 0.0,
            ..Allowances::modest()
        };
        let dear = Allowances {
            fuel_per_sec: cheap.fuel_per_sec * 100,
            memory_mib: cheap.memory_mib * 100,
            bytes_per_day: cheap.bytes_per_day * 100,
            ..cheap
        };
        let state = stand_in::chain(1_000_000_000_000, 0);
        state.lock().unwrap().allowances = chain::allowances(&cheap);
        let client = stand_in::client(&stand_in::start(state.clone()).await).await;
        let vault = Address::repeat_byte(0x11);
        let totals: Totals = Default::default();
        let tank = Tank::default();
        let through = tank.signed_through.clone();
        let started = Instant::now();
        let meter = tokio::spawn(run(
            client.clone(),
            stand_in::key(&client),
            vault,
            None,
            totals.clone(),
            tank,
            Arc::new(AtomicBool::new(true)),
            Ceilings::default(),
            0,
        ));

        let before = stand_in::signed_total(&totals, vault, None).await;
        assert!(
            through.covers(started),
            "a signed total doesn't say what time it counts up to"
        );
        state.lock().unwrap().down = true;
        tokio::time::sleep(Duration::from_secs(3)).await;
        {
            let mut state = state.lock().unwrap();
            state.allowances = chain::allowances(&dear);
            state.down = false;
        }
        let after = stand_in::signed_total(&totals, vault, Some(before)).await;
        // A reading at the new allowances is billed at them.
        let later = stand_in::signed_total(&totals, vault, Some(after)).await;
        meter.abort();

        let billed = (after - before) as f64;
        // At most four seconds and a half at the cheap rate, generously.
        let fair = 4.5 * per_second(&cheap);
        assert!(
            billed <= fair,
            "billed {billed} for the outage; at the allowances in force it is at most {fair}"
        );
        assert!(
            (later - after) as f64 >= 0.4 * per_second(&Ceilings::default().hold(&dear)),
            "the new allowances were never billed"
        );
    }

    /// The author asks for nearly all the memory a 32-bit count can name.
    /// The agent is given 4 GiB, and that is what the tank is billed for.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_allowance_past_its_ceiling_is_billed_at_the_ceiling() {
        let asked = Allowances {
            memory_mib: u32::MAX,
            gas_per_day: 0.0,
            ..Allowances::modest()
        };
        let held = Ceilings::default().hold(&asked);
        assert_eq!(held.memory_mib, Ceilings::MEMORY_MIB);
        let state = stand_in::chain(1_000_000_000_000, 0);
        state.lock().unwrap().allowances = chain::allowances(&asked);
        let client = stand_in::client(&stand_in::start(state.clone()).await).await;
        let vault = Address::repeat_byte(0x11);
        let totals: Totals = Default::default();
        let tank = Tank::default();
        let meter = tokio::spawn(run(
            client.clone(),
            stand_in::key(&client),
            vault,
            None,
            totals.clone(),
            tank,
            Arc::new(AtomicBool::new(true)),
            Ceilings::default(),
            0,
        ));
        let started = Instant::now();
        let first = stand_in::signed_total(&totals, vault, None).await;
        let second = stand_in::signed_total(&totals, vault, Some(first)).await;
        let seconds = started.elapsed().as_secs_f64();
        meter.abort();

        let billed = (second - first) as f64;
        // At most the time between the two, generously, at the ceiling: far
        // short of what holding what was asked would cost.
        let fair = (seconds + 1.0) * per_second(&held);
        assert!(
            billed <= fair,
            "billed {billed} in {seconds:.1}s; held to the ceiling it is at most {fair}"
        );
        assert!(billed < 0.01 * per_second(&asked) * seconds.max(0.5));
        assert!(billed > 0.0, "nothing was billed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_restarted_meter_carries_on_from_a_total_its_predecessor_had_collected() {
        let state = stand_in::chain(1_000_000_000, 0);
        state.lock().unwrap().allowances = chain::allowances(&accruing());
        let url = stand_in::start(state.clone()).await;
        let vault = Address::repeat_byte(0x22);
        let totals: Totals = Default::default();
        let running = || Arc::new(AtomicBool::new(true));

        // The first instance serves two totals, then dies before the last is
        // collected.
        let client = stand_in::client(&url).await;
        let first = tokio::spawn(run(
            client.clone(),
            stand_in::key(&client),
            vault,
            Some(2_500.0),
            totals.clone(),
            Tank::default(),
            running(),
            Ceilings::default(),
            0,
        ));
        let one = stand_in::signed_total(&totals, vault, None).await;
        let served = stand_in::signed_total(&totals, vault, Some(one)).await;
        first.abort();
        totals.lock().unwrap().clear();

        // The second takes the vault on from what the vault had collected,
        // which is nothing yet.
        let client = stand_in::client(&url).await;
        let second = tokio::spawn(run(
            client.clone(),
            stand_in::key(&client),
            vault,
            Some(2_500.0),
            totals.clone(),
            Tank::default(),
            running(),
            Ceilings::default(),
            0,
        ));
        let before = stand_in::signed_total(&totals, vault, None).await;
        assert!(
            before < served,
            "{before} is the second's own, from nothing"
        );

        // Then the first instance's last total is collected. It is carried
        // on from once a read blocks later finds it again.
        state.lock().unwrap().tank.usageSettled = U256::from(served);
        let read_once = stand_in::signed_total(&totals, vault, Some(before)).await;
        // Carried on from, it would be the collected figure and at least the
        // second's own accrual so far on top.
        assert!(
            read_once < served + before,
            "{read_once} carried on from a collected figure read only once"
        );
        state.lock().unwrap().head += AGREE_BLOCKS;
        let after = stand_in::signed_total(&totals, vault, Some(read_once)).await;
        second.abort();
        assert!(
            after > served,
            "the vault collected {served}; the next total signed was {after}"
        );
        assert!(
            after >= served + before,
            "the second's own accrual, {before}, is counted on top: {after}"
        );
    }
}
