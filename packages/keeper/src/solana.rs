//! The keeper on a Solana vault chain: every vault action anyone may send,
//! as on the EVM chains, through the vault program's own instructions.
//!
//! Each round, for each vault the vault program holds:
//!
//! - its bill collected from the tank with the executor's latest signed
//!   total, one `keep` per newer total, the executor asked once a day for an
//!   open vault, at the halt and once its grace is over for a halted one, and
//!   once a minute for a closed one in its last bill's window; a `keep`
//!   without a total once a day when the author keeps the tank above what it
//!   holds and it could be filled;
//! - a halted vault, once its grace is over, wound up: its station stopped,
//!   then closed in one transaction (stop, unwind, value, close) when that
//!   would go through, or else unwound only when a simulation shows the
//!   unwind brings money home, so the valuation after it finds the station
//!   empty; the close is held for the vault's last bill exactly as on the EVM
//!   (see [`collect_before_close`]);
//! - settlement of whatever the last priced valuation owes, in batches,
//!   before and after this slot's valuation, which is sent only when
//!   something waits for one (a request, the payday, a halted vault's close,
//!   or a day without one), it lands in the slot's window and a simulation
//!   shows it would not be void;
//! - every claim that is waiting tried, at most once a slot and less often
//!   the longer it keeps waiting; a vault whose claim is still unpaid after
//!   its try past the freeze period frozen; a retired vault evicted once its
//!   wind-down is over;
//! - each holder's units the pro-rata fallback left served (`serve_pro_rata`)
//!   at most once a slot, less often after one that landed and failed, never
//!   while a valuation's settlement is owed; a vault whose holder's units have
//!   waited unserved as long as a claim may frozen (`freeze_units`); a closed
//!   vault's units still served;
//! - once a day for an open vault that hasn't halted, after the slot's
//!   settlement, its restricted list read again (`refresh_restricted`) from
//!   each driver whose read alone would lengthen it, and only then;
//! - finished requests and claims closed, their rent going back to whoever
//!   paid it; a closed vault's waiting requests and claims paid through its
//!   own doors, its station stopped and emptied, its last bill collected.
//!
//! Every send is run by the node before it goes out (its preflight), so a
//! call the vault would refuse costs nothing; what must land inside a window,
//! or is only worth sending when it moves money, is simulated first as well,
//! as is every serve, freeze for units and refresh.
//! What the keeper remembers having done is remembered only once it landed
//! and went through.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::Result;
use chain::SolanaServed;
use chain::solana::platform::VaultAccounts;
use chain::solana::programs::anchor_lang::AccountDeserialize;
use chain::solana::programs::{station, vault as vp};
use chain::solana::rate_limited;
use chain::solana::scan::VaultScan;
use chain::solana::upkeep::{SignedUsage, queued_request};
use chain::solana::{
    Instruction, Keypair, Platform, Pubkey, Signer, SolanaClient, TransactionFailed,
};
use serde::Deserialize;
use vp::state::{Claim, ClaimState, Holder, Request, Vault, VaultState};

use crate::{DAY, MINUTE, Said, chain_kept, waited};

/// How long the keeper waits between one round of a cluster and the next.
const ROUND_PAUSE: Duration = Duration::from_secs(2);
/// The most rounds a vault's requests go unread for their rent: they are
/// read again sooner whenever settlement has moved.
const RENT_EVERY: u64 = 30;
/// The most records closed in one transaction.
const CLOSES_AT_ONCE: usize = 8;
/// The least an unwind must bring home to be paid for: a cent.
const UNWIND_WORTH_AT_LEAST: u128 = 10_000;
/// The longest wait, in chain time, between simulations of an unwind found
/// not worth sending.
const MOST_SECONDS_BETWEEN_CHECKS: u64 = 5 * MINUTE;
/// The longest wait, in slots, between tries of a claim that keeps waiting.
const MOST_SLOTS_BETWEEN_TRIES: u64 = 64;
/// The longest wait, in chain time, after a `keep` that landed and failed.
const MOST_SECONDS_BETWEEN_REVERTS: u64 = 60 * MINUTE;
/// Rounds in a row a vault may fail before it is in the keeper's health.
const MOST_VAULT_ROUNDS_FAILING: u64 = 3;
/// How long the executor may answer no ask before the health says so.
const UNANSWERED_FOR: Duration = Duration::from_secs(15 * 60);
/// How long one ask of the executor may take, and how long a `close` waits
/// for an answer.
const ASK_TIMEOUT: Duration = Duration::from_secs(5);

/// Keep one Solana vault chain, round after round, for good.
pub async fn keep_chain(served: SolanaServed, executor: String, services: Vec<String>) {
    let chain = served.vault_chain;
    let keeper = match served.client.role(chain::Role::Keeper) {
        Ok(keeper) => keeper.insecure_clone(),
        Err(error) => {
            tracing::warn!(chain = %served.key, error = format!("{error:#}"), "no keeper key on this cluster; it is not kept");
            chain_kept(chain, |kept| kept.no_keeper = true);
            return;
        }
    };
    tracing::info!(chain = %served.key, keeper = %keeper.pubkey(), "the keeper is up");
    chain_kept(chain, |kept| kept.connected = Some(Instant::now()));
    // The executor's keys on this cluster: blessings relayed, lamports
    // given once the registry vouches for a key.
    let keys = tokio::spawn(crate::solana_keys::keep_keys(
        served.client.clone(),
        served.platform.clone(),
        served.key.to_string(),
        keeper.insecure_clone(),
        executor.clone(),
        false,
    ));
    let _keys = Aborting(keys);
    // The builder's and the reviewer's Solana keys, each on its own cadence.
    let _services: Vec<Aborting> = services
        .into_iter()
        .map(|service| {
            Aborting(tokio::spawn(crate::solana_keys::keep_keys(
                served.client.clone(),
                served.platform.clone(),
                served.key.to_string(),
                keeper.insecure_clone(),
                service,
                true,
            )))
        })
        .collect();
    let keeping = Keeping {
        client: served.client.clone(),
        platform: served.platform.clone(),
        keeper,
        executor,
    };
    let mut memory: HashMap<Pubkey, Memory> = HashMap::new();
    let mut scan = VaultScan::for_cluster(&served.client.cluster);
    let mut said = Said::default();
    let mut slowed = 0u32;
    loop {
        let started = Instant::now();
        let done = keeping.round(&mut scan, &mut memory).await;
        let now = Instant::now();
        match &done {
            // A round cut short by a node that limits the rate failed at
            // nothing: it is not counted, and the next waits longer, the
            // longer the node keeps asking.
            Err(error) if rate_limited(error) => {
                slowed = slowed.saturating_add(1);
                let pause = slowed_pause(slowed);
                tracing::debug!(chain = %served.key, seconds = pause.as_secs(), error = format!("{error:#}"), "the node limits the rate; the next round waits");
                tokio::time::sleep(pause).await;
                continue;
            }
            _ => slowed = 0,
        }
        match done {
            Ok(()) => {
                chain_kept(chain, |kept| {
                    kept.finished = Some(now);
                    kept.took = now.duration_since(started);
                    kept.failing = 0;
                });
                if said.over() {
                    tracing::info!(chain = %served.key, "the rounds finish again");
                }
            }
            Err(error) => {
                chain_kept(chain, |kept| kept.failing += 1);
                if let Some(times) = said.due(now) {
                    tracing::warn!(chain = %served.key, times, error = format!("{error:#}"), "the round did not finish");
                }
            }
        }
        tokio::time::sleep(ROUND_PAUSE).await;
    }
}

/// How long the next round waits after `times` rounds in a row cut short by
/// a node limiting the rate: five seconds, doubling, at most two minutes.
fn slowed_pause(times: u32) -> Duration {
    Duration::from_secs((5u64 << times.saturating_sub(1).min(5)).min(120))
}

/// Aborts the task when dropped.
struct Aborting(tokio::task::JoinHandle<()>);

impl Drop for Aborting {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// One cluster as the keeper keeps it.
struct Keeping {
    client: SolanaClient,
    platform: Platform,
    keeper: Keypair,
    executor: String,
}

/// What the keeper remembers about one vault between rounds.
#[derive(Default)]
struct Memory {
    /// The keeper's rounds this vault has been in.
    rounds: u64,
    /// The chain time of the last daily round.
    daily_at: Option<u64>,
    /// The `as_of` of the last total a `keep` went through with, or that was
    /// counted settled while the vault refused it: each total is collected
    /// at most once.
    bill_sent_as_of: u64,
    /// The `as_of` of the last total the vault refused.
    bill_refused_as_of: u64,
    /// When a `keep` carrying a total is next sent after one that landed and
    /// failed: the wait in seconds, and the chain time.
    keep_again: Option<(u64, u64)>,
    /// Rounds in a row the vault refused its bill, and the round last
    /// counted.
    bill_refused: u64,
    bill_refused_in: Option<u64>,
    bill_refused_said: Said,
    /// Whether the executor has been asked since the halt was seen, and
    /// since its grace was over.
    asked_at_halt: bool,
    asked_after_grace: bool,
    /// When the executor was last asked for a closed vault's last bill, in
    /// chain time and on the keeper's clock: a minute must pass on both.
    bill_asked_at: Option<(u64, Instant)>,
    /// When an ask that got no answer is made again: the wait, and the chain
    /// time and the keeper's own clock it must pass on both, so a fast chain
    /// clock can't have a silent executor asked every round.
    ask_again: Option<(u64, u64, Instant)>,
    /// When the close was first held for its bill, and the hold's last try
    /// first made, in chain time and on the keeper's clock; the grace end of
    /// the halt whose bill was written off.
    close_held_since: Option<(u64, Instant)>,
    last_try_since: Option<(u64, Instant)>,
    written_off_for: Option<u64>,
    /// When the vault was first seen closed; the first ask sent once its last
    /// bill's window was over, and whether its last try was made.
    closed_seen_at: Option<(u64, Instant)>,
    asked_past_window: Option<Instant>,
    tried_past_window: bool,
    /// The chain time a closed vault was last found with nothing to do.
    quiet_since: Option<u64>,
    /// Each waiting claim's next try: the slot, and the wait in slots.
    claims_tried: HashMap<u64, (u64, u64)>,
    /// The unchanged share count and next bounded partial-payment probe.
    /// Retained across rounds so a probe budget cannot starve small parts.
    claim_parts_next: HashMap<u64, (u128, u128)>,
    /// Each unit holder's next serve, by owner (see [`next_units_try`]).
    units_tried: HashMap<Pubkey, UnitsTry>,
    /// The slot the station was last unwound in, and when an unwind found
    /// not worth sending is next simulated: the chain time, and the wait.
    unwound_in: Option<u64>,
    unwind_checks: Option<(u64, u64)>,
    /// Whether the last simulation found the unwind not worth sending.
    unwind_idle: bool,
    /// The settlement cursor the records were last read for their rent at,
    /// and the round.
    rent_read: Option<(u64, u64)>,
    /// The vault's rounds in a row that did not finish, and their warning.
    rounds_failing: u64,
    round_said: Said,
}

/// What became of a send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// It landed and went through.
    Done,
    /// The node ran it first and the vault refused it: it cost nothing.
    Refused,
    /// The vault refused it as `Stale`, which a node behind the cluster also
    /// says.
    Stale,
    /// It landed and failed: it paid its fee.
    Reverted,
    /// It didn't go out, or nobody can say whether it landed.
    NotSent,
}

impl Outcome {
    fn of(result: &Result<chain::solana::Landed>) -> Self {
        let Err(error) = result else {
            return Outcome::Done;
        };
        if let Some(failed) = error
            .chain()
            .find_map(|c| c.downcast_ref::<TransactionFailed>())
        {
            if failed.custom == Some(vault_error(vp::error::VaultError::Stale)) {
                return Outcome::Stale;
            }
            return match failed.signature {
                Some(_) => Outcome::Reverted,
                None => Outcome::Refused,
            };
        }
        // It never landed, the node turned it away for its own reasons, or
        // nobody can say: nothing the vault said.
        Outcome::NotSent
    }
}

/// A vault program error as the node reports its code.
fn vault_error(error: vp::error::VaultError) -> u32 {
    error as u32 + chain::solana::programs::anchor_lang::error::ERROR_CODE_OFFSET
}

/// What became of collecting the bill with the executor's latest answer, as
/// on the EVM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Billing {
    NoAnswer,
    NotServed,
    UpToDate,
    Collected,
    Refused,
    NotCollected,
    EmptyTank,
    Capped,
}

impl Billing {
    /// Whether nothing is left to collect with the latest answer in hand.
    fn settled(self) -> bool {
        !matches!(self, Billing::Refused | Billing::NotCollected)
    }
}

/// Whether a `keep` with a total could only charge nothing: an empty tank,
/// or a collection that has reached the halt's grace end with no cap
/// carried. Either would only write the total off.
fn nothing_to_charge(tank: &vp::state::Tank, grace_end: Option<u64>) -> Option<Billing> {
    if tank.balance == 0 {
        Some(Billing::EmptyTank)
    } else if grace_end.is_some_and(|end| tank.last_collection >= end) && tank.cap_carried == 0 {
        Some(Billing::Capped)
    } else {
        None
    }
}

/// Whether a signed total bills anything the vault hasn't collected, and is
/// newer than the last one sent.
fn owes(signed: &SignedUsage, tank: &vp::state::Tank, sent_as_of: u64) -> bool {
    signed.total > tank.usage_settled
        && signed.as_of > tank.usage_as_of
        && signed.as_of > sent_as_of
}

/// Whether a valuation sent now lands inside this slot's window and the
/// vault hasn't been valued in it: the vault's own rule, with a second's
/// room for the transaction to land.
fn valuation_due(vault: &Vault, now: u64) -> bool {
    let interval = vault.timers.slot_interval.max(1);
    let start = now - now % interval;
    now - start < vault.timers.slot_window
        && (vault.valuation_count == 0 || vault.last_valued_at < start)
}

/// Why the vault wants a valuation in the slot starting at `slot`, if it
/// does: as on the EVM (see [`crate::valuation_wanted`]), each one costs a
/// transaction, so one is sent only when something waits for it. A request
/// a valuation in the slot would price that the last priced one didn't
/// (taken through the fallback or not: one valuation passes them all); the
/// weekly payday; a halted vault's close, once after the halt and on every
/// slot once its grace is over and its station is being emptied, until a
/// valuation finds it empty; and a day since the last priced valuation, or
/// none ever. The program keeps no close staged across transactions, so
/// nothing waits on one here.
fn valuation_wanted(vault: &Vault, slot: u64, grace_over: bool) -> Option<&'static str> {
    if !vault.priced {
        return Some("never priced");
    }
    if slot >= vault.latest.time.saturating_add(crate::HEARTBEAT) {
        return Some("a day since the last priced valuation");
    }
    if vault.state != VaultState::Closed && slot >= vp::maths::next_monday(vault.last_payment) {
        return Some("payday");
    }
    if vault.priced_by(slot) > vault.priced_through {
        return Some("a request waits for its price");
    }
    if vault.halted()
        && !vault.ready_to_close()
        && (vault.latest.time <= vault.halted_at || grace_over)
    {
        return Some("its close");
    }
    None
}

/// Why the vault wants a valuation sent at `now`, if it does (see
/// [`valuation_wanted`]): judged at the time the valuation is given, its
/// slot's start, with the halt's grace as it stands at `now`.
fn valuation_wanted_now(vault: &Vault, now: u64) -> Option<&'static str> {
    let grace_over =
        vault.halted() && now >= vault.halted_at.saturating_add(vault.timers.halt_grace);
    valuation_wanted(
        vault,
        slot_start(now, vault.timers.slot_interval),
        grace_over,
    )
}

/// The slot `now` falls in, counted from the first: the keeper's own count,
/// for when a claim or a holder's units are next tried.
fn slot_of(now: u64, interval: u64) -> u64 {
    now / interval.max(1)
}

/// When the slot `now` falls in begins, in the chain's seconds: the time a
/// valuation recorded in it is given, which the vault's own times are
/// compared with.
fn slot_start(now: u64, interval: u64) -> u64 {
    now - now % interval.max(1)
}

/// The next try of a claim, after one at `slot` left it `state`: a claim
/// the market kept waiting is tried ever less often, up to
/// [`MOST_SLOTS_BETWEEN_TRIES`] slots apart; any other next slot.
fn next_try(before: Option<(u64, u64)>, state: ClaimState, slot: u64) -> (u64, u64) {
    let wait = match (state, before) {
        (ClaimState::Waiting, Some((_, wait))) => (wait * 2).clamp(1, MOST_SLOTS_BETWEEN_TRIES),
        _ => 1,
    };
    (slot + wait, wait)
}

/// The next simulation of an unwind found not worth sending: a slot on,
/// doubling up to [`MOST_SECONDS_BETWEEN_CHECKS`].
fn next_check(before: Option<(u64, u64)>, now: u64, interval: u64) -> (u64, u64) {
    let wait = before
        .map(|(_, wait)| wait * 2)
        .unwrap_or(interval)
        .clamp(interval.max(1), MOST_SECONDS_BETWEEN_CHECKS.max(interval));
    (now + wait, wait)
}

/// Whether an unwind that brings home `brought` is worth paying for: a cent,
/// or the vault's dust, whichever is larger.
/// The drivers, of `count`, a stopped unwind names: grown from none a
/// driver at a time in the station's order, each kept only if `goes` says
/// the set with it goes through.
async fn grown<F, Fut>(count: u8, mut goes: F) -> Vec<u8>
where
    F: FnMut(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let mut named = Vec::new();
    for index in 0..count {
        let mut with = named.clone();
        with.push(index);
        if goes(with.clone()).await {
            named = with;
        }
    }
    named
}

/// The most parts of one claim paid in a round, and the most parts of it
/// simulated.
const CLAIM_PARTS_A_ROUND: u32 = 4;
const CLAIM_PART_TRIES_A_ROUND: u32 = 16;

/// The parts of a claim of `shares` tried, in order: half of it, then half
/// of that, down to a single share; never all of it (a whole try is
/// `settle_claim`'s), never none.
fn halvings(shares: u128, next: Option<u128>) -> impl Iterator<Item = u128> {
    let first = next
        .filter(|part| *part > 0 && *part < shares)
        .unwrap_or(shares / 2);
    std::iter::successors(Some(first), |part| Some(part / 2)).take_while(|part| *part > 0)
}

/// Whether a decoded claim proves payment progress. A settlement keeps the
/// account until the separate rent duty closes it; a missing account is no
/// evidence of a successful simulation.
fn served(before: u128, before_paid: u64, after: Option<(ClaimState, u128, u64)>) -> bool {
    match after {
        Some((ClaimState::Paid, 0, paid)) => before > 0 && paid > before_paid,
        Some((ClaimState::Pending, shares, paid)) => {
            shares > 0 && shares < before && paid > before_paid
        }
        _ => false,
    }
}

/// A whole settlement can queue the claim against available cash or
/// complete a dust claim; those ordinary paths must not become part closes.
fn settlement_progress(
    before: u128,
    before_paid: u64,
    after: (ClaimState, u128, u64),
    whole: bool,
) -> bool {
    (whole
        && before > 0
        && after.2 >= before_paid
        && (matches!(after, (ClaimState::Queued, shares, _) if shares == before)
            || matches!(after, (ClaimState::Paid, shares, _) if shares <= before)))
        || served(before, before_paid, Some(after))
}

/// The most stopped unwinds of one vault the keeper sends in one slot.
const UNWINDS_A_SLOT: u32 = 4;

/// Whether, `done` unwinds of a stopped station sent in this slot, another
/// that would bring `brought` home is sent: while it is worth an unwind and
/// fewer than [`UNWINDS_A_SLOT`] went.
fn unwind_again(done: u32, brought: u128, dust: u128) -> bool {
    done < UNWINDS_A_SLOT && worth_an_unwind(brought, dust)
}

fn worth_an_unwind(brought: u128, dust: u128) -> bool {
    brought >= UNWIND_WORTH_AT_LEAST.max(dust)
}

/// When a holder's units are next tried.
#[derive(Clone, Copy, Debug, PartialEq)]
struct UnitsTry {
    /// The first slot they may be tried in.
    next: u64,
    /// How many slots the last send that landed and failed was told to wait.
    wait: u64,
    /// The first of the keeper's rounds (see [`Memory::rounds`]) they may be
    /// tried in: a cluster whose clock runs fast passes many slots in a
    /// round, and the wait is whichever of the two is longer.
    next_round: u64,
}

/// Whether a holder's units are tried in `slot` and the keeper's round
/// `round`, after the try `before`.
fn units_due(before: Option<UnitsTry>, slot: u64, round: u64) -> bool {
    before.is_none_or(|before| slot >= before.next && round >= before.next_round)
}

/// The next try of a holder's units, after one at `slot` in the keeper's
/// round `round` that came to `outcome` (none when nothing was sent), the
/// vault's slots `interval` apart: a send that landed and failed paid its fee
/// and served nothing, so the wait doubles, up to
/// [`MOST_SECONDS_BETWEEN_REVERTS`], and is as many of the keeper's rounds
/// too; anything else is tried again next slot, the wait kept, as a refusal
/// costs nothing.
fn next_units_try(
    before: Option<UnitsTry>,
    outcome: Option<Outcome>,
    slot: u64,
    interval: u64,
    round: u64,
) -> UnitsTry {
    let kept = before.map_or(1, |before| before.wait);
    match outcome {
        Some(Outcome::Reverted) => {
            let most = (MOST_SECONDS_BETWEEN_REVERTS / interval.max(1)).max(1);
            let wait = before.map_or(1, |before| (before.wait * 2).clamp(1, most));
            UnitsTry {
                next: slot + wait,
                wait,
                next_round: round + wait,
            }
        }
        _ => UnitsTry {
            next: slot + 1,
            wait: kept,
            next_round: round,
        },
    }
}

/// Whether a vault may be frozen for `holder`'s units: they have waited
/// unserved as long as a claim may, the vault is live and hasn't halted, and
/// no valuation's settlement is owed, which the serve waits for and may pay
/// them from.
fn freeze_for_units_due(holder: &Holder, vault: &Vault, now: u64) -> bool {
    holder.units != 0
        && now >= holder.units_since.saturating_add(vault.timers.freeze_after)
        && vault.live()
        && !vault.halted()
        && !vault.owed()
}

/// Whether the keeper tries a claim in `state` this round: one pending or
/// waiting is tried; one the cash covered (queued) waits for the next
/// valuation to pay it, and is only looked at for its freeze.
fn claim_tried(state: ClaimState) -> bool {
    matches!(state, ClaimState::Pending | ClaimState::Waiting)
}

/// Whether a vault may be frozen for a claim in `state` booked at
/// `booked_at`: it is still unpaid (pending, waiting or queued) a freeze
/// period after its booking, the vault is live and hasn't halted, and no
/// valuation's settlement is owed, which may pay it.
fn freeze_for_claim_due(state: ClaimState, booked_at: u64, vault: &Vault, now: u64) -> bool {
    matches!(
        state,
        ClaimState::Pending | ClaimState::Waiting | ClaimState::Queued
    ) && now >= booked_at.saturating_add(vault.timers.freeze_after)
        && vault.live()
        && !vault.halted()
        && !vault.owed()
}

/// Whether a vault's restricted list is read again in its daily round: while
/// it is open and hasn't halted, as on the EVM.
fn refresh_wanted(vault: &Vault) -> bool {
    vault.state == VaultState::Open && !vault.halted()
}

/// Whether a refresh left the vault, as simulated, with a longer list than
/// the one whose hash it holds: one that adds nothing is not paid for.
fn lengthens(before: &Vault, after: Option<&Vault>) -> bool {
    after.is_some_and(|after| after.restricted_hash != before.restricted_hash)
}

impl Keeping {
    fn keeper(&self) -> Pubkey {
        self.keeper.pubkey()
    }

    /// Send `ixs` as the keeper, and say what became of it. A send the
    /// cluster never took is said at warn and in the health until the same
    /// call goes through.
    async fn send(&self, vault: &Pubkey, ixs: Vec<Instruction>, what: &'static str) -> Outcome {
        let result = self.client.send(ixs, &[&self.keeper]).await;
        let outcome = Outcome::of(&result);
        let key = (*vault, what);
        match (&result, outcome) {
            (_, Outcome::NotSent) => {
                let error = result
                    .as_ref()
                    .err()
                    .map(|e| format!("{e:#}"))
                    .unwrap_or_default();
                let due = TROUBLES.lock().ok().and_then(|mut troubles| {
                    troubles
                        .not_sent
                        .entry(key)
                        .or_insert_with(|| (Instant::now(), Said::default()))
                        .1
                        .due(Instant::now())
                });
                if let Some(times) = due {
                    tracing::warn!(%vault, what, times, error, "the keeper's send did not go out");
                }
            }
            (Err(error), _) => {
                went_out(&key);
                tracing::debug!(%vault, what, error = format!("{error:#}"), "not yet");
            }
            (Ok(landed), _) => {
                went_out(&key);
                tracing::info!(%vault, what, signature = landed.signature(), "done");
            }
        }
        outcome
    }

    /// Simulate `ixs` as the keeper's, and send them only when the node
    /// accepts them: one the node refuses cost nothing and is counted
    /// refused (or stale, or not sent when the node itself failed).
    async fn send_simulated(
        &self,
        vault: &Pubkey,
        ixs: Vec<Instruction>,
        what: &'static str,
    ) -> Outcome {
        match self.client.simulate(ixs.clone(), &self.keeper()).await {
            Ok(_) => self.send(vault, ixs, what).await,
            Err(error) => {
                tracing::debug!(%vault, what, error = format!("{error:#}"), "the simulation turns it down; not sent");
                Outcome::of(&Err(error))
            }
        }
    }

    async fn round(
        &self,
        scan: &mut VaultScan,
        memory: &mut HashMap<Pubkey, Memory>,
    ) -> Result<()> {
        // Every vault and every claim of each, read once a round; the vault
        // program listed only when its counts say something new is there.
        let found = scan.look(&self.client).await?;
        let vaults = found.vaults;
        let addresses: Vec<Pubkey> = vaults.iter().map(|(address, _)| *address).collect();
        let mut claims: BTreeMap<Pubkey, Vec<Claim>> = BTreeMap::new();
        for (vault, _, claim) in found.claims {
            claims.entry(vault).or_default().push(claim);
        }
        for (address, vault) in &vaults {
            let remembered = memory.entry(*address).or_default();
            let theirs = claims.remove(address).unwrap_or_default();
            let done = self.keep_one(address, vault, &theirs, remembered).await;
            steps_worked(address, done.is_ok());
            match done {
                Ok(()) => {
                    remembered.rounds_failing = 0;
                    if remembered.round_said.over() {
                        tracing::info!(vault = %address, "the vault's rounds finish again");
                    }
                    if let Ok(mut troubles) = TROUBLES.lock() {
                        troubles.unkept.remove(address);
                        troubles.limited.remove(address);
                    }
                }
                // The node asking to be asked less often is no failure of
                // the vault's, but a vault left round after round is not
                // kept all the same: it counts as a round that didn't
                // finish, said only once it has held as many rounds as a
                // failure must to be in the health.
                Err(error) if rate_limited(&error) => {
                    remembered.rounds_failing += 1;
                    if remembered.rounds_failing < MOST_VAULT_ROUNDS_FAILING {
                        tracing::debug!(vault = %address, error = format!("{error:#}"), "the node limits the rate; the vault's round is left to the next");
                    } else if let Some(times) = remembered.round_said.due(Instant::now()) {
                        tracing::warn!(vault = %address, times, rounds = remembered.rounds_failing, error = format!("{error:#}"), "the node's rate limit keeps the vault's round from finishing");
                    }
                    if let Ok(mut troubles) = TROUBLES.lock() {
                        troubles.unkept.insert(*address, remembered.rounds_failing);
                        troubles.limited.insert(*address);
                    }
                }
                Err(error) => {
                    remembered.rounds_failing += 1;
                    if let Some(times) = remembered.round_said.due(Instant::now()) {
                        tracing::warn!(vault = %address, times, error = format!("{error:#}"), "the vault's round did not finish");
                    }
                    if let Ok(mut troubles) = TROUBLES.lock() {
                        troubles.unkept.insert(*address, remembered.rounds_failing);
                        troubles.limited.remove(address);
                    }
                }
            }
        }
        if let Ok(mut troubles) = TROUBLES.lock() {
            troubles.unkept.retain(|vault, _| addresses.contains(vault));
            troubles.limited.retain(|vault| addresses.contains(vault));
            troubles
                .bills_refused
                .retain(|vault, _| addresses.contains(vault));
            troubles
                .not_sent
                .retain(|(vault, _), _| addresses.contains(vault));
        }
        if let Ok(mut failed) = STEPS_FAILED.lock() {
            failed.retain(|(vault, _), _| addresses.contains(vault));
        }
        if let Ok(mut asks) = ASKS.lock() {
            asks.unanswered.retain(|vault| addresses.contains(vault));
            asks.not_wanted.retain(|vault, _| addresses.contains(vault));
            asks.latest.retain(|vault, _| addresses.contains(vault));
            asks.note_serving();
        }
        say_serving_none();
        Ok(())
    }

    /// Everything one vault might need this round.
    async fn keep_one(
        &self,
        address: &Pubkey,
        vault: &Vault,
        claims: &[Claim],
        memory: &mut Memory,
    ) -> Result<()> {
        memory.rounds += 1;
        let now = self.client.now().await?;
        let v = self.platform.vault_accounts(vault.strategy_id);
        if vault.state == VaultState::Closed {
            if memory
                .quiet_since
                .is_some_and(|at| now < at.saturating_add(DAY))
            {
                return Ok(());
            }
            return self.closed_round(&v, vault, claims, now, memory).await;
        }
        let grace_end = vault
            .halted()
            .then(|| vault.halted_at.saturating_add(vault.timers.halt_grace));

        // The bill: whatever newer total the executor has answered with is
        // collected, once; the executor is asked off the round.
        let billing = self
            .collect_answered(&v, vault, grace_end, now, false, memory)
            .await;
        let due = memory.daily_at.is_none_or(|at| now >= at + DAY);
        // The restricted list is the day's too, read after the settle.
        let refresh = due && refresh_wanted(vault);
        if due {
            memory.daily_at = Some(now);
            if !vault.halted() && nothing_to_charge(&vault.tank, None).is_none() {
                ask(&self.client.cluster, &self.executor, &v.vault);
            }
            if !vault.halted() && billing != Billing::Collected && self.fill_wanted(&v, vault).await
            {
                carry_on(
                    address,
                    "queue a tank fill",
                    async {
                        let ixs = self
                            .platform
                            .ix_keep(&self.client, &v, vault, &self.keeper(), None)
                            .await?;
                        self.send(address, ixs, "keep").await;
                        Ok(())
                    }
                    .await,
                );
            }
        }
        if let Some(grace_end) = grace_end {
            self.ask_halted(&v, vault, now, grace_end, memory);
        }
        self.ask_again(&v.vault, now, memory);

        // A halted vault's station is stopped and emptied before the slot's
        // valuation, so the valuation finds it empty and the vault can close.
        let mut closed = false;
        if let Some(grace_end) = grace_end
            && now >= grace_end
        {
            match self
                .stop_and_unwind(&v, address, grace_end, now, memory)
                .await
            {
                Ok(done) => closed = done,
                Err(error) => carry_on(address, "stop and unwind the station", Err(error)),
            }
        }
        if closed {
            return Ok(());
        }

        carry_on(address, "settle", self.settle(&v).await);
        carry_on(address, "value", self.value(&v, address).await);
        carry_on(address, "settle", self.settle(&v).await);
        // After the round's settle, so the list is read against what was
        // priced.
        if refresh {
            carry_on(
                address,
                "refresh the restricted list",
                self.refresh_restricted(address).await,
            );
        }
        carry_on(
            address,
            "claims",
            self.claims(&v, address, claims, memory, (vault, now)).await,
        );
        carry_on(
            address,
            "units",
            self.units(&v, address, true, memory).await.map(|_| ()),
        );

        if vault.state == VaultState::WindingDown
            && !vault.halted()
            && now >= vault.retired_at.saturating_add(vault.timers.wind_down)
        {
            self.send(address, vec![self.platform.ix_evict(address)], "evict")
                .await;
        }

        // `close` has no window: it goes whenever a valuation after the halt
        // found the station empty and nothing priced is owed. The bill is
        // collected first; a bill that didn't go through holds the close.
        let vault = self.platform.vault(&self.client, address).await?;
        if let Some(grace_end) = grace_end
            && vault.ready_to_close()
            && !vault.owed()
            && self.would_close(&v, &vault).await
            && self
                .collect_before_close(&v, &vault, grace_end, now, memory)
                .await
        {
            self.send(
                address,
                vec![self.platform.ix_close_vault(&v, &vault)],
                "close",
            )
            .await;
        }
        carry_on(address, "take back rent", self.rent(address, memory).await);
        Ok(())
    }

    /// Whether a fill could be queued and paid: the author keeps the tank
    /// above what it holds, none is queued, the author holds shares and the
    /// vault holds cash.
    async fn fill_wanted(&self, v: &VaultAccounts, vault: &Vault) -> bool {
        if vault.tank.keep_at <= vault.tank.balance || vault.tank.fill_pending || vault.cash == 0 {
            return false;
        }
        self.platform
            .shares(&self.client, &v.vault, &vault.author)
            .await
            .is_ok_and(|shares| shares > 0)
    }

    /// Whether the vault's `close` would go through now.
    async fn would_close(&self, v: &VaultAccounts, vault: &Vault) -> bool {
        self.client
            .simulate(vec![self.platform.ix_close_vault(v, vault)], &self.keeper())
            .await
            .is_ok()
    }

    /// Settle everything the last priced valuation owes.
    async fn settle(&self, v: &VaultAccounts) -> Result<()> {
        let sent = self
            .platform
            .settle_all(&self.client, &self.keeper, v)
            .await?;
        if sent > 0 {
            tracing::info!(vault = %v.vault, batches = sent, "settled");
        }
        Ok(())
    }

    /// This slot's valuation, when something waits for one (see
    /// [`valuation_wanted`]), it lands inside the window and a simulation
    /// shows it would price: a void one is not paid for.
    async fn value(&self, v: &VaultAccounts, address: &Pubkey) -> Result<()> {
        let vault = self.platform.vault(&self.client, address).await?;
        let now = self.client.now().await?;
        if !valuation_due(&vault, now) || vault.owed() {
            return Ok(());
        }
        let Some(why) = valuation_wanted_now(&vault, now) else {
            return Ok(());
        };
        tracing::debug!(vault = %address, why, "a valuation is wanted");
        let ix = self.platform.ix_record_valuation(&self.client, v).await?;
        let simulated = match self
            .client
            .simulate_reading(vec![ix.clone()], &self.keeper(), &[*address])
            .await
        {
            Ok(simulated) => simulated,
            // Outside the window, or valued already: next slot.
            Err(error) => {
                tracing::debug!(vault = %address, error = format!("{error:#}"), "no valuation this slot");
                return Ok(());
            }
        };
        let after = simulated
            .accounts
            .first()
            .cloned()
            .flatten()
            .and_then(|account| Vault::try_deserialize(&mut &account.data[..]).ok());
        let priced =
            after.is_some_and(|after| after.priced && after.latest.index == vault.valuation_count);
        if !priced {
            tracing::debug!(vault = %address, "this slot's valuation would be void; not sent");
            return Ok(());
        }
        self.send(address, vec![ix], "value").await;
        Ok(())
    }

    /// Try every claim that is waiting, at most once a slot and less often
    /// the longer it keeps waiting; freeze the vault for one still unpaid
    /// after its try past the freeze period.
    async fn claims(
        &self,
        v: &VaultAccounts,
        address: &Pubkey,
        claims: &[Claim],
        memory: &mut Memory,
        round: (&Vault, u64),
    ) -> Result<()> {
        memory
            .claims_tried
            .retain(|id, _| claims.iter().any(|claim| claim.id == *id));
        memory.claim_parts_next.retain(|id, _| {
            claims
                .iter()
                .any(|claim| claim.id == *id && claim_tried(claim.state))
        });
        for claim in claims {
            let tried = claim_tried(claim.state);
            if !tried && claim.state != ClaimState::Queued {
                continue;
            }
            if !tried {
                // The cash covered it: the next valuation pays it, so it is
                // not tried again. Still unpaid past the freeze period, it
                // freezes the vault as any other unpaid claim does: checked
                // first against this round's reads, then read afresh, since
                // the round's own settle may have paid it since.
                if !freeze_for_claim_due(claim.state, claim.booked_at, round.0, round.1) {
                    continue;
                }
                let now = self.client.now().await?;
                let vault = self.platform.vault(&self.client, address).await?;
                if let Some(fresh) = self.platform.claim(&self.client, address, claim.id).await? {
                    self.freeze_if_overdue(
                        address,
                        claim.id,
                        fresh.state,
                        fresh.booked_at,
                        &vault,
                        now,
                    )
                    .await;
                }
                continue;
            }
            let now = self.client.now().await?;
            let vault = self.platform.vault(&self.client, address).await?;
            let slot = slot_of(now, vault.timers.slot_interval);
            let before = memory.claims_tried.get(&claim.id).copied();
            if before.is_some_and(|(next, _)| slot < next) {
                continue;
            }
            // Whole when a whole try would pay; otherwise in parts while a
            // part would (a venue may close only so much in one
            // transaction); and a whole try sent all the same when none
            // would, so the claim is tried as before.
            let paid_parts = if self.claim_serves(v, address, claim, None).await {
                0
            } else {
                self.claim_in_parts(v, address, claim.id, memory).await
            };
            let outcome = if paid_parts > 0 {
                Outcome::Done
            } else {
                let ix = self
                    .platform
                    .ix_settle_claim(&self.client, v, &self.keeper(), claim.id)
                    .await?;
                self.send(address, vec![ix], "settle a claim").await
            };
            let after = self.platform.claim(&self.client, address, claim.id).await?;
            let state = after.as_ref().map_or(ClaimState::Paid, |claim| claim.state);
            match outcome {
                // Refused before it went out cost nothing: next slot.
                Outcome::Refused | Outcome::NotSent | Outcome::Stale => {
                    memory.claims_tried.insert(claim.id, (slot + 1, 1));
                }
                _ => {
                    memory
                        .claims_tried
                        .insert(claim.id, next_try(before, state, slot));
                }
            }
            if state == ClaimState::Paid {
                tracing::info!(vault = %address, claim = claim.id, "the claim is paid");
            }
            // Tried and still unpaid past the freeze period: the vault is
            // frozen, never while a valuation's settlement is owed.
            let booked_at = after
                .as_ref()
                .map_or(claim.booked_at, |claim| claim.booked_at);
            self.freeze_if_overdue(address, claim.id, state, booked_at, &vault, now)
                .await;
        }
        Ok(())
    }

    /// Freeze the vault for claim `id` (see [`freeze_for_claim_due`]).
    async fn freeze_if_overdue(
        &self,
        address: &Pubkey,
        id: u64,
        state: ClaimState,
        booked_at: u64,
        vault: &Vault,
        now: u64,
    ) {
        if freeze_for_claim_due(state, booked_at, vault, now) {
            self.send(
                address,
                vec![self.platform.ix_freeze(address, id)],
                "freeze",
            )
            .await;
        }
    }

    /// Whether a try of claim `claim` (whole, or `part` of its shares) would
    /// pay something, from a simulation of it: the claim paid, or fewer of
    /// its shares left.
    async fn claim_serves(
        &self,
        v: &VaultAccounts,
        address: &Pubkey,
        claim: &Claim,
        part: Option<u128>,
    ) -> bool {
        let ix = match part {
            None => {
                self.platform
                    .ix_settle_claim(&self.client, v, &self.keeper(), claim.id)
                    .await
            }
            Some(shares) => {
                self.platform
                    .ix_settle_claim_part(&self.client, v, &self.keeper(), claim.id, shares)
                    .await
            }
        };
        let Ok(ix) = ix else {
            return false;
        };
        let at = vp::state::claim_address(address, claim.id).0;
        let Ok(simulated) = self
            .client
            .simulate_reading(vec![ix], &self.keeper(), &[at])
            .await
        else {
            return false;
        };
        let [Some(account)] = simulated.accounts.as_slice() else {
            return false;
        };
        if account.owner != vp::ID {
            return false;
        }
        let Ok(after) = Claim::try_deserialize(&mut &account.data[..]) else {
            return false;
        };
        if after.id != claim.id || after.owner != claim.owner || after.paid < claim.paid {
            return false;
        }
        settlement_progress(
            claim.shares,
            claim.paid,
            (after.state, after.shares, after.paid),
            part.is_none(),
        )
    }

    /// Claim `id` served in parts: halving its shares from half until a part
    /// would pay, that part sent, and again on what is left while each part
    /// is paid, at most [`CLAIM_PARTS_A_ROUND`] parts a round. Answers how
    /// many parts were paid.
    async fn claim_in_parts(
        &self,
        v: &VaultAccounts,
        address: &Pubkey,
        id: u64,
        memory: &mut Memory,
    ) -> u32 {
        let mut paid = 0;
        let mut tried = 0;
        while paid < CLAIM_PARTS_A_ROUND && tried < CLAIM_PART_TRIES_A_ROUND {
            let Ok(Some(claim)) = self.platform.claim(&self.client, address, id).await else {
                break;
            };
            if !matches!(claim.state, ClaimState::Pending | ClaimState::Waiting) {
                break;
            }
            // What remains after a paid part may now fit a whole settlement.
            if paid > 0 && self.claim_serves(v, address, &claim, None).await {
                let Ok(ix) = self
                    .platform
                    .ix_settle_claim(&self.client, v, &self.keeper(), id)
                    .await
                else {
                    break;
                };
                if self
                    .send(address, vec![ix], "settle the rest of a claim")
                    .await
                    == Outcome::Done
                {
                    if let Ok(Some(after)) = self.platform.claim(&self.client, address, id).await
                        && after.id == claim.id
                        && after.owner == claim.owner
                        && after.paid >= claim.paid
                        && served(
                            claim.shares,
                            claim.paid,
                            Some((after.state, after.shares, after.paid)),
                        )
                    {
                        paid += 1;
                    }
                    memory.claim_parts_next.remove(&id);
                }
                break;
            }
            let next = memory
                .claim_parts_next
                .get(&id)
                .filter(|(basis, _)| *basis == claim.shares)
                .map(|(_, next)| *next);
            let mut sent = false;
            for part in halvings(claim.shares, next) {
                if tried >= CLAIM_PART_TRIES_A_ROUND {
                    break;
                }
                tried += 1;
                memory.claim_parts_next.insert(id, (claim.shares, part));
                if !self.claim_serves(v, address, &claim, Some(part)).await {
                    memory.claim_parts_next.insert(id, (claim.shares, part / 2));
                    continue;
                }
                let Ok(ix) = self
                    .platform
                    .ix_settle_claim_part(&self.client, v, &self.keeper(), id, part)
                    .await
                else {
                    break;
                };
                if self.send(address, vec![ix], "settle part of a claim").await == Outcome::Done {
                    let Ok(Some(after)) = self.platform.claim(&self.client, address, id).await
                    else {
                        break;
                    };
                    if after.id != claim.id
                        || after.owner != claim.owner
                        || after.paid < claim.paid
                        || !served(
                            claim.shares,
                            claim.paid,
                            Some((after.state, after.shares, after.paid)),
                        )
                    {
                        break;
                    }
                    memory.claim_parts_next.remove(&id);
                    paid += 1;
                    sent = true;
                    tracing::info!(vault = %address, claim = id, shares = part, "part of the claim is paid");
                }
                break;
            }
            if !sent {
                break;
            }
        }
        paid
    }

    /// Finished requests and claims closed, their rent going back to whoever
    /// paid it, read again whenever settlement has moved.
    async fn rent(&self, address: &Pubkey, memory: &mut Memory) -> Result<()> {
        let vault = self.platform.vault(&self.client, address).await?;
        let read_at = (vault.request_cursor, memory.rounds);
        if memory.rent_read.is_some_and(|(cursor, round)| {
            cursor == vault.request_cursor && memory.rounds < round + RENT_EVERY
        }) {
            return Ok(());
        }
        let mut closes = Vec::new();
        for (record, request) in self.platform.requests_of(&self.client, address).await? {
            if Platform::request_closable(&vault, &request) {
                closes.push(self.platform.ix_close_request(address, &record, &request));
            }
        }
        let claims = self.platform.claims_of(&self.client, &[*address]).await?;
        for (_, _, claim) in &claims {
            if Platform::claim_closable(&vault, claim) {
                closes.push(self.platform.ix_close_claim(address, claim));
            }
        }
        let mut all_done = true;
        for batch in closes.chunks(CLOSES_AT_ONCE) {
            if self.send(address, batch.to_vec(), "take back rent").await != Outcome::Done {
                all_done = false;
            }
        }
        if all_done {
            memory.rent_read = Some(read_at);
        }
        Ok(())
    }

    // The pro-rata fallback and the restricted list ------------------------------------------

    /// Serve every holder's units the pro-rata fallback left, each at most
    /// once a slot and less often after a serve that landed and failed; and,
    /// if `freezable`, freeze the vault for units left unserved as long as a
    /// claim may wait. Holders are read only while the vault counts units.
    /// Whether every holder is served.
    async fn units(
        &self,
        v: &VaultAccounts,
        address: &Pubkey,
        freezable: bool,
        memory: &mut Memory,
    ) -> Result<bool> {
        let vault = self.platform.vault(&self.client, address).await?;
        if vault.units == 0 {
            memory.units_tried.clear();
            return Ok(true);
        }
        let holders = self.platform.unit_holders(&self.client, address).await?;
        memory
            .units_tried
            .retain(|owner, _| holders.iter().any(|holder| holder.owner == *owner));
        let mut all_served = true;
        for holder in &holders {
            // One holder whose serve can't be built or read holds up no
            // other.
            match self
                .serve_holder(v, address, &holder.owner, freezable, memory)
                .await
            {
                Ok(served) => all_served &= served,
                Err(error) => {
                    all_served = false;
                    carry_on(address, "serve a holder's units", Err(error));
                }
            }
        }
        Ok(all_served)
    }

    /// One holder's units served, when their try is due; the vault frozen
    /// for them if they are still unserved and have waited as long as a claim
    /// may. Whether they are served.
    async fn serve_holder(
        &self,
        v: &VaultAccounts,
        address: &Pubkey,
        owner: &Pubkey,
        freezable: bool,
        memory: &mut Memory,
    ) -> Result<bool> {
        let now = self.client.now().await?;
        let vault = self.platform.vault(&self.client, address).await?;
        let slot = slot_of(now, vault.timers.slot_interval);
        let before = memory.units_tried.get(owner).copied();
        if !units_due(before, slot, memory.rounds) {
            return Ok(false);
        }
        // Refused while a valuation's settlement is owed: not sent.
        let mut outcome = None;
        if !vault.owed() {
            let ix = self
                .platform
                .ix_serve_pro_rata(&self.client, v, &vault, &self.keeper(), owner)
                .await?;
            outcome = Some(
                self.send_simulated(address, vec![ix], "serve a holder's units")
                    .await,
            );
        }
        let left = self
            .platform
            .holder(&self.client, address, owner)
            .await?
            .filter(|holder| holder.units != 0);
        let Some(holder) = left else {
            memory.units_tried.remove(owner);
            tracing::info!(vault = %address, %owner, "the holder's units are served");
            return Ok(true);
        };
        if freezable {
            let vault = self.platform.vault(&self.client, address).await?;
            if freeze_for_units_due(&holder, &vault, now) {
                let ix = self.platform.ix_freeze_units(address, owner);
                let frozen = self
                    .send_simulated(address, vec![ix], "freeze for units left unserved")
                    .await;
                if frozen == Outcome::Reverted {
                    outcome = Some(Outcome::Reverted);
                }
            }
        }
        memory.units_tried.insert(
            *owner,
            next_units_try(
                before,
                outcome,
                slot,
                vault.timers.slot_interval,
                memory.rounds,
            ),
        );
        Ok(false)
    }

    /// The vault's restricted list read again from its drivers: each driver
    /// is simulated alone, so one whose read fails (which fails the whole
    /// call) is left out, and only those that would lengthen the list are
    /// sent, together, once the node accepts them.
    async fn refresh_restricted(&self, address: &Pubkey) -> Result<()> {
        let vault = self.platform.vault(&self.client, address).await?;
        if !refresh_wanted(&vault) {
            return Ok(());
        }
        let now = self.client.now().await?;
        let mut adding = Vec::new();
        for (index, name) in vault.drivers.iter().enumerate() {
            let index = u8::try_from(index)?;
            let reading = self
                .platform
                .driver_reading(&self.client, name, now)
                .await?;
            let ix =
                self.platform
                    .ix_refresh_restricted(address, &self.keeper(), &[(index, reading)]);
            let after = match self
                .client
                .simulate_reading(vec![ix], &self.keeper(), &[*address])
                .await
            {
                Ok(simulated) => simulated
                    .accounts
                    .first()
                    .cloned()
                    .flatten()
                    .and_then(|account| Vault::try_deserialize(&mut &account.data[..]).ok()),
                Err(error) => {
                    tracing::debug!(vault = %address, driver = index, error = format!("{error:#}"), "the driver's list can't be read again; left out");
                    None
                }
            };
            if lengthens(&vault, after.as_ref()) {
                adding.push((index, reading));
            }
        }
        if adding.is_empty() {
            tracing::debug!(vault = %address, "the restricted list is current");
            return Ok(());
        }
        let ix = self
            .platform
            .ix_refresh_restricted(address, &self.keeper(), &adding);
        if self
            .send_simulated(address, vec![ix], "refresh the restricted list")
            .await
            == Outcome::Done
        {
            tracing::info!(vault = %address, drivers = adding.len(), "the restricted list was read again from the drivers");
        }
        Ok(())
    }

    // The halt -------------------------------------------------------------------------------

    /// An ask that got no answer is made again a chain-minute later, then
    /// after two, four and so on up to an hour, until one is answered.
    fn ask_again(&self, vault: &Pubkey, now: u64, memory: &mut Memory) {
        let unanswered = ASKS
            .lock()
            .is_ok_and(|asks| asks.unanswered.contains(vault));
        if !unanswered {
            memory.ask_again = None;
            return;
        }
        let later = |wait: u64| {
            Some((
                wait,
                now.saturating_add(wait),
                Instant::now() + Duration::from_secs(wait),
            ))
        };
        match memory.ask_again {
            None => memory.ask_again = later(MINUTE),
            Some((wait, next, next_here)) if now >= next && Instant::now() >= next_here => {
                if ask(&self.client.cluster, &self.executor, vault).is_some() {
                    memory.ask_again = later((wait * 2).min(60 * MINUTE));
                }
            }
            Some(_) => {}
        }
    }

    /// Ask for a halted vault's total when the halt is first seen and again
    /// once its grace is over.
    fn ask_halted(
        &self,
        v: &VaultAccounts,
        vault: &Vault,
        now: u64,
        grace_end: u64,
        memory: &mut Memory,
    ) {
        if nothing_to_charge(&vault.tank, Some(grace_end)).is_some() {
            return;
        }
        let asked = if now < grace_end {
            &mut memory.asked_at_halt
        } else {
            &mut memory.asked_after_grace
        };
        if !*asked && ask(&self.client.cluster, &self.executor, &v.vault).is_some() {
            *asked = true;
        }
    }

    /// A halted vault past its grace: its station stopped, then closed in one
    /// transaction when that would go through, the bill collected first;
    /// otherwise its station unwound when a simulation shows the unwind
    /// brings money home. Whether the vault closed.
    async fn stop_and_unwind(
        &self,
        v: &VaultAccounts,
        address: &Pubkey,
        grace_end: u64,
        now: u64,
        memory: &mut Memory,
    ) -> Result<bool> {
        let station = self.platform.station(&self.client, &v.station).await?;
        // Whatever was priced before the stop is settled first, so the
        // close in one transaction can follow.
        self.settle(v).await?;
        let vault = self.platform.vault(&self.client, address).await?;
        let slot = slot_of(now, vault.timers.slot_interval);

        // The close in one transaction, which nobody can get between, when
        // it lands in this slot's window and would go through.
        if !vault.owed() && valuation_due(&vault, now) {
            let ixs = self
                .platform
                .ixs_close_in_one(&self.client, v, &vault, &self.keeper(), station.stopped)
                .await?;
            let fits = self
                .client
                .wire_size(ixs.clone(), &self.keeper())
                .is_ok_and(|size| size <= chain::solana::client::PACKET_DATA_SIZE);
            if fits
                && self
                    .client
                    .simulate(ixs.clone(), &self.keeper())
                    .await
                    .is_ok()
            {
                if !self
                    .collect_before_close(v, &vault, grace_end, now, memory)
                    .await
                {
                    return Ok(false);
                }
                // The bill may have taken a while: judged again on the clock.
                let vault = self.platform.vault(&self.client, address).await?;
                if !valuation_due(&vault, self.client.now().await?) {
                    return Ok(false);
                }
                let ixs = self
                    .platform
                    .ixs_close_in_one(&self.client, v, &vault, &self.keeper(), station.stopped)
                    .await?;
                if self.send(address, ixs, "close in one transaction").await == Outcome::Done {
                    tracing::info!(vault = %address, "the halted vault is closed");
                    return Ok(true);
                }
                return Ok(false);
            }
        }

        if !station.stopped {
            self.send(address, vec![self.platform.ix_stop(v)], "stop the station")
                .await;
        }
        self.unwind(v, address, &vault, now, slot, memory).await?;
        Ok(false)
    }

    /// The stopped station unwound in full, at most once a slot, only when a
    /// simulation shows it brings home a cent or the vault's dust, whichever
    /// is larger. One found not worth it is simulated again ever less often.
    async fn unwind(
        &self,
        v: &VaultAccounts,
        address: &Pubkey,
        vault: &Vault,
        now: u64,
        slot: u64,
        memory: &mut Memory,
    ) -> Result<()> {
        if memory.unwound_in == Some(slot)
            || memory.unwind_checks.is_some_and(|(next, _)| now < next)
        {
            return Ok(());
        }
        let station = self.platform.station(&self.client, &v.station).await?;
        if !station.stopped {
            return Ok(());
        }
        let Some((ix, brought)) = self.next_unwind(v, address, &station).await else {
            memory.unwind_checks = Some(next_check(
                memory.unwind_checks,
                now,
                vault.timers.slot_interval,
            ));
            memory.unwind_idle = true;
            return Ok(());
        };
        if !worth_an_unwind(brought, vault.dust()) {
            memory.unwind_checks = Some(next_check(
                memory.unwind_checks,
                now,
                vault.timers.slot_interval,
            ));
            memory.unwind_idle = true;
            return Ok(());
        }
        memory.unwind_checks = None;
        memory.unwind_idle = false;
        if self.send(address, vec![ix], "unwind the station").await != Outcome::Done {
            return Ok(());
        }
        memory.unwound_in = Some(slot);
        tracing::info!(vault = %address, brought, "the stopped station was unwound");
        // A venue may close only part of a crowded holding in one unwind
        // (Orca's sale stops at the ticks one transaction can cross): the
        // rest is unwound again in the same slot while each one still brings
        // something worth it home.
        let mut done = 1;
        loop {
            let station = self.platform.station(&self.client, &v.station).await?;
            let Some((ix, brought)) = self.next_unwind(v, address, &station).await else {
                break;
            };
            if !unwind_again(done, brought, vault.dust()) {
                break;
            }
            if self
                .send(address, vec![ix], "unwind the station again")
                .await
                != Outcome::Done
            {
                break;
            }
            done += 1;
            tracing::info!(vault = %address, brought, done, "the stopped station was unwound again");
        }
        Ok(())
    }

    /// The stopped station's full unwind as it stands now and what it would
    /// send home, without the drivers that would fail it; `None` if nothing
    /// would go through.
    async fn next_unwind(
        &self,
        v: &VaultAccounts,
        address: &Pubkey,
        station: &station::state::Station,
    ) -> Option<(chain::solana::Instruction, u128)> {
        let full = self
            .platform
            .ix_unwind(
                &self.client,
                v,
                &self.keeper(),
                chain::solana::programs::driver::WAD,
            )
            .await;
        let tried = match full {
            Ok(ix) => self
                .brings(v, station, &ix)
                .await
                .map(|brought| (ix, brought)),
            Err(error) => Err(error),
        };
        match tried {
            Ok(tried) => Some(tried),
            Err(error) => {
                tracing::debug!(vault = %address, error = format!("{error:#}"), "the unwind would not go through; trying it without the drivers that fail");
                self.unwind_without_failing(v, station).await
            }
        }
    }

    /// What `ix`, an unwind of the stopped station, would send home, from a
    /// simulation of it.
    async fn brings(
        &self,
        v: &VaultAccounts,
        station: &station::state::Station,
        ix: &chain::solana::Instruction,
    ) -> Result<u128> {
        let simulated = self
            .client
            .simulate_reading(vec![ix.clone()], &self.keeper(), &[v.station])
            .await?;
        Ok(simulated
            .accounts
            .first()
            .cloned()
            .flatten()
            .and_then(|account| {
                station::state::Station::try_deserialize(&mut &account.data[..]).ok()
            })
            .map_or(0, |after| after.sent_home.saturating_sub(station.sent_home)))
    }

    /// The stopped station's unwind through as many of its drivers as go
    /// through together, leaving out those that would fail it, and what it
    /// would send home; `None` if even sending the counted dollars home
    /// would not go through. The set is grown a driver at a time, each kept
    /// only if the set with it still goes through, so a driver that fails
    /// only beside another (the two too large for one transaction, say)
    /// still leaves the rest to come home.
    async fn unwind_without_failing(
        &self,
        v: &VaultAccounts,
        station: &station::state::Station,
    ) -> Option<(chain::solana::Instruction, u128)> {
        let named = grown(station.drivers.len() as u8, |with| async move {
            match self
                .platform
                .ix_unwind_stopped(&self.client, v, &self.keeper(), &with)
                .await
            {
                Ok(ix) => self.brings(v, station, &ix).await.is_ok(),
                Err(_) => false,
            }
        })
        .await;
        let ix = self
            .platform
            .ix_unwind_stopped(&self.client, v, &self.keeper(), &named)
            .await
            .ok()?;
        let brought = self.brings(v, station, &ix).await.ok()?;
        if named.len() < station.drivers.len() {
            tracing::warn!(vault = %v.vault, named = ?named, "a driver's close fails: the stopped station is unwound without it");
        }
        Some((ix, brought))
    }

    // The bill --------------------------------------------------------------------------------

    /// Collect the bill with the latest answer the executor has given, when
    /// it bills anything the vault could charge. Each total goes through in
    /// one `keep` at most; one that landed and failed is tried again after a
    /// wait, unless this is the last try (see [`collect_before_close`]).
    async fn collect_answered(
        &self,
        v: &VaultAccounts,
        vault: &Vault,
        grace_end: Option<u64>,
        now: u64,
        last_try: bool,
        memory: &mut Memory,
    ) -> Billing {
        let (billing, _) = self
            .collect_latest(v, vault, grace_end, now, last_try, memory)
            .await;
        match billing {
            Billing::NoAnswer | Billing::NotCollected => {}
            Billing::Refused => {
                if memory.bill_refused_in != Some(memory.rounds) {
                    memory.bill_refused_in = Some(memory.rounds);
                    memory.bill_refused += 1;
                    if let Ok(mut troubles) = TROUBLES.lock() {
                        troubles.bills_refused.insert(v.vault, memory.bill_refused);
                    }
                    if let Some(times) = memory.bill_refused_said.due(Instant::now()) {
                        tracing::warn!(vault = %v.vault, times, "the vault refuses the keep carrying its bill; it is tried again");
                    }
                }
            }
            _ => {
                if bill_settled(&v.vault, memory) {
                    tracing::info!(vault = %v.vault, "the vault's bill no longer waits on a refused keep");
                }
            }
        }
        billing
    }

    /// [`Keeping::collect_answered`] before what became of it is noted; also
    /// when the ask the answer is for was sent.
    async fn collect_latest(
        &self,
        v: &VaultAccounts,
        vault: &Vault,
        grace_end: Option<u64>,
        now: u64,
        last_try: bool,
        memory: &mut Memory,
    ) -> (Billing, Option<Instant>) {
        if let Some(nothing) = nothing_to_charge(&vault.tank, grace_end) {
            total_not_wanted(&v.vault);
            return (nothing, None);
        }
        let Some((answer, asked)) = latest_answer(&v.vault) else {
            return (Billing::NoAnswer, None);
        };
        let Asked::Total(signed) = answer else {
            return (Billing::NotServed, Some(asked));
        };
        if !owes(&signed, &vault.tank, memory.bill_sent_as_of) {
            return (Billing::UpToDate, Some(asked));
        }
        if memory.keep_again.is_some_and(|(_, next)| now < next)
            && (!last_try || memory.bill_refused_as_of == signed.as_of)
        {
            return (Billing::Refused, Some(asked));
        }
        let ixs = match self
            .platform
            .ix_keep(&self.client, v, vault, &self.keeper(), Some(&signed))
            .await
        {
            Ok(ixs) => ixs,
            Err(error) => {
                carry_on(&v.vault, "build the keep", Err(error));
                return (Billing::NotCollected, Some(asked));
            }
        };
        let billing = match self.send(&v.vault, ixs, "keep").await {
            Outcome::Done => Billing::Collected,
            Outcome::Reverted => {
                let wait = memory
                    .keep_again
                    .map_or(vault.timers.slot_interval, |(wait, _)| wait * 2)
                    .clamp(
                        vault.timers.slot_interval.max(1),
                        MOST_SECONDS_BETWEEN_REVERTS,
                    );
                memory.keep_again = Some((wait, now + wait));
                memory.bill_refused_as_of = signed.as_of;
                return (Billing::Refused, Some(asked));
            }
            Outcome::Refused => {
                memory.bill_refused_as_of = signed.as_of;
                return (Billing::Refused, Some(asked));
            }
            Outcome::NotSent | Outcome::Stale => return (Billing::NotCollected, Some(asked)),
        };
        tracing::info!(vault = %v.vault, total = signed.total, as_of = signed.as_of, "the bill was collected");
        memory.keep_again = None;
        memory.bill_sent_as_of = signed.as_of;
        (billing, Some(asked))
    }

    /// Before a send that may close a halted vault, whose author may take
    /// the tank the moment it closes: the executor is asked, its answer
    /// waited for up to [`ASK_TIMEOUT`], and whatever newer total it gives
    /// collected. Whether the close may go, by the EVM's rules: once nothing
    /// is left to collect; a refused or unsent `keep` holds the close for at
    /// most fifteen minutes on both clocks, then gets one last try, after
    /// which the bill is written off and the close goes, depositors first.
    async fn collect_before_close(
        &self,
        v: &VaultAccounts,
        vault: &Vault,
        grace_end: u64,
        now: u64,
        memory: &mut Memory,
    ) -> bool {
        let written_off = memory.written_off_for == Some(grace_end);
        let last_try = !written_off
            && memory
                .close_held_since
                .is_some_and(|since| waited(since, now));
        if nothing_to_charge(&vault.tank, Some(grace_end)).is_none() {
            ask(&self.client.cluster, &self.executor, &v.vault);
            answer_in_flight(&v.vault, ASK_TIMEOUT).await;
        }
        let billing = self
            .collect_answered(v, vault, Some(grace_end), now, last_try, memory)
            .await;
        if written_off {
            return true;
        }
        if billing.settled() {
            bill_settled(&v.vault, memory);
            return true;
        }
        if last_try {
            let refused = billing == Billing::Refused;
            let tried_out = memory
                .last_try_since
                .is_some_and(|since| waited(since, now));
            if refused || tried_out {
                tracing::warn!(vault = %v.vault, refused, "the bill has held the vault's close as long as it may; the vault is closed with it uncollected");
                memory.bill_sent_as_of = memory.bill_sent_as_of.max(memory.bill_refused_as_of);
                bill_settled(&v.vault, memory);
                memory.written_off_for = Some(grace_end);
                return true;
            }
            memory.last_try_since.get_or_insert((now, Instant::now()));
        }
        memory.close_held_since.get_or_insert((now, Instant::now()));
        tracing::debug!(vault = %v.vault, "the bill did not go through; the close waits for it");
        false
    }

    // A closed vault --------------------------------------------------------------------------

    /// A closed vault: its station stopped once the grace is over and asked
    /// to send home what it still holds, what was waiting when it closed paid
    /// through its own doors, its last bill collected, its records closed for
    /// their rent. One with nothing left to do is looked at once a day.
    async fn closed_round(
        &self,
        v: &VaultAccounts,
        vault: &Vault,
        claims: &[Claim],
        now: u64,
        memory: &mut Memory,
    ) -> Result<()> {
        let address = v.vault;
        let grace_end = vault.halted_at.saturating_add(vault.timers.halt_grace);
        let station = self.platform.station(&self.client, &v.station).await?;
        let mut station_left = !station.stopped;
        if !station.stopped && now >= grace_end {
            self.send(
                &address,
                vec![self.platform.ix_stop(v)],
                "stop the closed vault's station",
            )
            .await;
        }
        if station.stopped {
            let slot = slot_of(now, vault.timers.slot_interval);
            carry_on(
                &address,
                "empty the closed vault's station",
                self.unwind(v, &address, vault, now, slot, memory).await,
            );
            station_left = !memory.unwind_idle;
        }

        // What was waiting when it closed, paid out. A queued claim names
        // the request it is queued by, found among the vault's by its id.
        let mut waiting = false;
        let requests: Result<Vec<(Pubkey, Request)>> =
            self.platform.requests_of(&self.client, &address).await;
        for claim in claims {
            if matches!(
                claim.state,
                ClaimState::Pending | ClaimState::Waiting | ClaimState::Queued
            ) {
                waiting = true;
                let queued_by = queued_request(&address, claim, requests.as_deref().unwrap_or(&[]));
                if claim.state == ClaimState::Queued && queued_by.is_none() {
                    // Not read yet: the next round reads the requests again.
                    tracing::debug!(vault = %address, claim = claim.id, "the request a queued claim is queued by wasn't found");
                    continue;
                }
                self.send(
                    &address,
                    vec![self.platform.ix_claim_closed(v, claim, queued_by.as_ref())],
                    "pay a claim of the closed vault",
                )
                .await;
            }
        }
        let requests = requests?;
        for (record, request) in &requests {
            if !request.done && request.kind != vp::state::RequestKind::Claim {
                waiting = true;
                self.send(
                    &address,
                    vec![self.platform.ix_settle_closed(v, vault, record, request)],
                    "settle a request of the closed vault",
                )
                .await;
            }
        }

        // A closed vault is past freezing; its units are still served.
        match self.units(v, &address, false, memory).await {
            Ok(served) => waiting |= !served,
            Err(error) => {
                waiting = true;
                carry_on(&address, "units", Err(error));
            }
        }

        let closed_at = *memory.closed_seen_at.get_or_insert((now, Instant::now()));
        let billed = self
            .last_bill(v, vault, closed_at, grace_end, now, memory)
            .await;
        carry_on(
            &address,
            "take back rent",
            self.rent(&address, memory).await,
        );
        if !station_left && !waiting && billed {
            memory.quiet_since = Some(now);
        }
        Ok(())
    }

    /// A closed vault's last bill, and whether it is settled: the executor is
    /// asked once a minute and each newer total collected; for fifteen
    /// minutes (both clocks) past the later of the close and the grace's end
    /// it is never settled; after that, once an answer to an ask sent since
    /// says there is nothing more, or the vault can charge nothing.
    async fn last_bill(
        &self,
        v: &VaultAccounts,
        vault: &Vault,
        closed_at: (u64, Instant),
        grace_end: u64,
        now: u64,
        memory: &mut Memory,
    ) -> bool {
        let over = waited((closed_at.0.max(grace_end), closed_at.1), now);
        let past_window = over
            && latest_answer(&v.vault)
                .zip(memory.asked_past_window)
                .is_some_and(|((_, answered), past)| answered >= past);
        let last_try = past_window && !memory.tried_past_window;
        let (billing, answered) = self
            .collect_latest(v, vault, Some(grace_end), now, last_try, memory)
            .await;
        if matches!(billing, Billing::EmptyTank | Billing::Capped) {
            return over;
        }
        if memory.bill_asked_at.is_none_or(|(at, here)| {
            now >= at.saturating_add(MINUTE) && here.elapsed() >= Duration::from_secs(MINUTE)
        }) && let Some(sent) = ask(&self.client.cluster, &self.executor, &v.vault)
        {
            memory.bill_asked_at = Some((now, Instant::now()));
            if over && memory.asked_past_window.is_none() {
                memory.asked_past_window = Some(sent);
            }
        }
        let settled = past_window
            && answered
                .zip(memory.asked_past_window)
                .is_some_and(|(answered, past)| answered >= past)
            && matches!(
                billing,
                Billing::NotServed | Billing::UpToDate | Billing::Collected | Billing::Refused
            );
        if settled {
            memory.tried_past_window = true;
            total_not_wanted(&v.vault);
            memory.bill_sent_as_of = memory.bill_sent_as_of.max(memory.bill_refused_as_of);
            bill_settled(&v.vault, memory);
        }
        settled
    }
}

/// A step that failed is said at warn (at once, then at most every ten
/// minutes with a count, for each vault and step) and the round carries on:
/// one that can't go through this round mustn't hold up the steps after it.
/// A round of the vault that finishes without it failing ends it (see
/// [`steps_worked`]): its next failure is said at once.
fn carry_on(vault: &Pubkey, step: &'static str, done: Result<()>) {
    if let Err(error) = done {
        let due = STEPS_FAILED.lock().ok().and_then(|mut failed| {
            let failed = failed.entry((*vault, step)).or_default();
            failed.this_round = true;
            failed.said.due(Instant::now())
        });
        if let Some(times) = due {
            tracing::warn!(%vault, step, times, error = format!("{error:#}"), "a step did not go through");
        }
    }
}

/// The vault's round is over: if it `finished`, the steps that failed before
/// and not in it go through again. Either way the next round starts afresh.
fn steps_worked(vault: &Pubkey, finished: bool) {
    let Ok(mut failed) = STEPS_FAILED.lock() else {
        return;
    };
    failed.retain(|(at, step), failed| {
        if at != vault {
            return true;
        }
        if finished && !failed.this_round {
            if failed.said.over() {
                tracing::info!(vault = %at, step, "the step goes through again");
            }
            return false;
        }
        failed.this_round = false;
        true
    });
}

/// The steps that failed, by vault and step (see [`carry_on`]).
static STEPS_FAILED: std::sync::LazyLock<
    std::sync::Mutex<HashMap<(Pubkey, &'static str), crate::StepFailed>>,
> = std::sync::LazyLock::new(Default::default);

/// The vault's bill is settled: the vault is no longer said to refuse it,
/// and a close held for it is held no longer. Whether it had been said.
fn bill_settled(vault: &Pubkey, memory: &mut Memory) -> bool {
    memory.close_held_since = None;
    memory.last_try_since = None;
    memory.bill_refused = 0;
    if let Ok(mut troubles) = TROUBLES.lock() {
        troubles.bills_refused.remove(vault);
    }
    memory.bill_refused_said.over()
}

/// A call went out (whatever the vault made of it): it is no longer said
/// not to go out.
fn went_out(key: &(Pubkey, &'static str)) {
    if let Ok(mut troubles) = TROUBLES.lock()
        && troubles.not_sent.remove(key).is_some()
    {
        tracing::info!(vault = %key.0, what = key.1, "the keeper's send goes out again");
    }
}

// Asking the executor -----------------------------------------------------------------------------

/// What the executor answered for a vault's total.
#[derive(Clone)]
enum Asked {
    Total(SignedUsage),
    NotServed,
}

/// The executor's asks, by vault: the latest answer and when the ask it
/// answers was sent, the ask in flight, and since when no ask has been
/// answered while some go unanswered.
#[derive(Default)]
struct Asks {
    latest: HashMap<Pubkey, (Asked, Instant)>,
    in_flight: HashMap<Pubkey, Instant>,
    unanswered: std::collections::BTreeSet<Pubkey>,
    /// When the keeper last said it no longer wants a vault's total: an
    /// answer to an ask sent before then doesn't count (see
    /// [`total_not_wanted`]).
    not_wanted: HashMap<Pubkey, Instant>,
    failing_since: Option<Instant>,
    said: Said,
    /// Since when its latest answer for every vault the keeper still wants a
    /// total for has been a 404, while it is (see [`serving_none`]), and
    /// that warning.
    serving_none_since: Option<Instant>,
    serving_none_said: Said,
}

impl Asks {
    /// Whether the keeper still wants a total for the vault, as of an ask
    /// sent at `sent`.
    fn wanted(&self, vault: &Pubkey, sent: Instant) -> bool {
        self.not_wanted.get(vault).is_none_or(|since| sent > *since)
    }

    /// Start or stop the time the executor has served no vault (see
    /// [`serving_none`]), counting only the vaults the keeper still wants a
    /// total for: it serves none while every such vault's latest answer is a
    /// 404, and not once any gets a total or none is wanted.
    fn note_serving(&mut self) {
        let mut wanted = self
            .latest
            .iter()
            .filter(|(vault, (_, sent))| self.wanted(vault, *sent))
            .peekable();
        let none = wanted.peek().is_some()
            && wanted.all(|(_, (asked, _))| matches!(asked, Asked::NotServed));
        if none {
            self.serving_none_since.get_or_insert_with(Instant::now);
        } else {
            self.serving_none_since = None;
        }
    }
}

/// Whether the executor's latest answer for every Solana vault the keeper
/// still wants a total for has been a 404, with none served a total, for as
/// long as the EVM's executors may (see [`crate::serving_none_for`]): a URL
/// that is wrong answers so, and no bill is collected.
fn serving_none(asks: &Asks, now: Instant) -> bool {
    asks.serving_none_since
        .is_some_and(|since| now.saturating_duration_since(since) >= crate::serving_none_for())
}

/// Say at warn that the executor serves no Solana vault (see
/// [`serving_none`]), at once, then at most every ten minutes with a count,
/// while it doesn't.
fn say_serving_none() {
    let Ok(mut asks) = ASKS.lock() else {
        return;
    };
    let now = Instant::now();
    if serving_none(&asks, now) {
        if let Some(times) = asks.serving_none_said.due(now) {
            tracing::warn!(
                times,
                "the executor has answered 404 for every Solana vault it was asked about, and served none; check the executor's URL"
            );
        }
    } else if asks.serving_none_said.over() {
        tracing::info!("the executor serves a Solana vault again");
    }
}

static ASKS: std::sync::LazyLock<std::sync::Mutex<Asks>> =
    std::sync::LazyLock::new(Default::default);

static HTTP: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(ASK_TIMEOUT)
        .build()
        .unwrap_or_default()
});

/// Ask the executor for a vault's total, off the round; its answer is kept
/// whenever it comes back. Not while an ask for the vault is in flight. When
/// the ask was sent, if it was.
fn ask(cluster: &str, url: &str, vault: &Pubkey) -> Option<Instant> {
    let mut asks = ASKS.lock().ok()?;
    if asks.in_flight.contains_key(vault) {
        return None;
    }
    let sent = Instant::now();
    asks.in_flight.insert(*vault, sent);
    let (cluster, url, vault) = (cluster.to_string(), url.to_string(), *vault);
    tokio::spawn(async move {
        let answer = fetch_total(&cluster, &url, &vault).await;
        let Ok(mut asks) = ASKS.lock() else {
            return;
        };
        if asks.in_flight.get(&vault) == Some(&sent) {
            asks.in_flight.remove(&vault);
        }
        match answer {
            Ok(asked) => {
                asks.unanswered.remove(&vault);
                asks.failing_since = None;
                if asks.said.over() {
                    tracing::info!("the executor answers again");
                }
                if matches!(asked, Asked::NotServed) && !asks.latest.contains_key(&vault) {
                    tracing::warn!(%vault, "the executor serves no total for this vault; if it should, check the executor's URL");
                }
                if asks.latest.get(&vault).is_none_or(|(_, at)| *at < sent) {
                    asks.latest.insert(vault, (asked, sent));
                }
                asks.note_serving();
            }
            Err(error) => {
                if asks
                    .not_wanted
                    .get(&vault)
                    .is_some_and(|since| sent <= *since)
                {
                    return;
                }
                asks.unanswered.insert(vault);
                let now = Instant::now();
                asks.failing_since.get_or_insert(now);
                if let Some(times) = asks.said.due(now) {
                    tracing::warn!(%vault, times, error, "the executor could not be asked for the vault's total; its bill waits for the next ask");
                }
            }
        }
    });
    Some(sent)
}

fn latest_answer(vault: &Pubkey) -> Option<(Asked, Instant)> {
    ASKS.lock().ok()?.latest.get(vault).cloned()
}

/// Wait for the vault's ask in flight to come back, for at most `limit`.
async fn answer_in_flight(vault: &Pubkey, limit: Duration) {
    let started = Instant::now();
    while started.elapsed() < limit
        && ASKS
            .lock()
            .is_ok_and(|asks| asks.in_flight.contains_key(vault))
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The keeper no longer wants a total for the vault: it no longer counts
/// against the executor in the health, and an ask already sent that comes
/// back with no answer doesn't put it back.
fn total_not_wanted(vault: &Pubkey) {
    if let Ok(mut asks) = ASKS.lock() {
        asks.unanswered.remove(vault);
        asks.not_wanted.insert(*vault, Instant::now());
        if asks.unanswered.is_empty() {
            asks.failing_since = None;
        }
        asks.note_serving();
    }
}

/// A running total as the executor serves it for a Solana vault.
#[derive(Deserialize)]
struct Served {
    total: serde_json::Value,
    as_of: u64,
    signature: String,
    signer: String,
}

/// Read what the executor served: the total as a decimal string or a
/// number, the signature in base58 or `0x` hex, the signer in base58.
fn read_served(served: Served) -> Result<SignedUsage, String> {
    let total = match &served.total {
        serde_json::Value::String(text) => text
            .parse::<u64>()
            .map_err(|e| format!("a total that is no number: {e}"))?,
        serde_json::Value::Number(number) => number.as_u64().ok_or("a total that is no u64")?,
        _ => return Err("a total that is no number".to_string()),
    };
    let bytes = match served.signature.strip_prefix("0x") {
        Some(hex) => {
            alloy::hex::decode(hex).map_err(|e| format!("a signature that is not hex: {e}"))?
        }
        None => bs58::decode(&served.signature)
            .into_vec()
            .map_err(|e| format!("a signature that is not base58: {e}"))?,
    };
    let signature: [u8; 64] = bytes
        .try_into()
        .map_err(|_| "a signature that is not 64 bytes".to_string())?;
    let signer: Pubkey = served
        .signer
        .parse()
        .map_err(|_| "a signer that is not an address".to_string())?;
    Ok(SignedUsage {
        total,
        as_of: served.as_of,
        signer,
        signature,
    })
}

async fn fetch_total(cluster: &str, executor: &str, vault: &Pubkey) -> Result<Asked, String> {
    let reply = HTTP
        .get(format!("{executor}/usage/{cluster}/{vault}"))
        .send()
        .await
        .map_err(|error| error.without_url().to_string())?;
    if reply.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Asked::NotServed);
    }
    if !reply.status().is_success() {
        return Err(format!("answered {}", reply.status()));
    }
    let served: Served = reply
        .json()
        .await
        .map_err(|error| format!("not a total: {}", error.without_url()))?;
    read_served(served).map(Asked::Total)
}

// Health ---------------------------------------------------------------------------------------------

/// What the keeper's health hears of its Solana vaults.
#[derive(Default)]
struct Troubles {
    /// Vaults whose rounds haven't finished, and how many in a row.
    unkept: HashMap<Pubkey, u64>,
    /// Of those, the vaults whose latest round was cut short by the node's
    /// rate limit.
    limited: HashSet<Pubkey>,
    /// Vaults that refuse the `keep` carrying their bill, and for how many
    /// rounds.
    bills_refused: HashMap<Pubkey, u64>,
    /// Sends that didn't go out, by vault and what they were for, since
    /// when, and their warning.
    not_sent: HashMap<(Pubkey, &'static str), (Instant, Said)>,
}

static TROUBLES: std::sync::LazyLock<std::sync::Mutex<Troubles>> =
    std::sync::LazyLock::new(Default::default);

/// The keeper's Solana troubles, as the health says them: a short phrase for
/// the first line, and the whole of it.
pub fn troubles(now: Instant) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(troubles) = TROUBLES.lock() {
        let mut unkept: Vec<String> = troubles
            .unkept
            .iter()
            .filter(|(_, failing)| **failing >= MOST_VAULT_ROUNDS_FAILING)
            .map(|(vault, failing)| {
                let why = if troubles.limited.contains(vault) {
                    ", the node's rate limit"
                } else {
                    ""
                };
                format!("solana/{vault} ({failing} rounds in a row{why})")
            })
            .collect();
        if !unkept.is_empty() {
            unkept.sort_unstable();
            out.push((
                format!("{} vaults not kept", unkept.len()),
                format!("vault rounds not finishing: {}", unkept.join(", ")),
            ));
        }
        if !troubles.not_sent.is_empty() {
            let mut whats: Vec<String> = troubles
                .not_sent
                .keys()
                .map(|(vault, what)| format!("{what} solana/{vault}"))
                .collect();
            whats.sort_unstable();
            out.push((
                format!("{} sends not going out", whats.len()),
                format!("not going out: {}", whats.join(", ")),
            ));
        }
        let mut refused: Vec<String> = troubles
            .bills_refused
            .iter()
            .map(|(vault, rounds)| format!("solana/{vault} ({rounds} rounds in a row)"))
            .collect();
        if !refused.is_empty() {
            refused.sort_unstable();
            out.push((
                format!("{} bills refused", refused.len()),
                format!("bills the vault refuses to collect: {}", refused.join(", ")),
            ));
        }
    }
    if let Ok(asks) = ASKS.lock()
        && asks
            .failing_since
            .is_some_and(|since| now.saturating_duration_since(since) >= UNANSWERED_FOR)
        && !asks.unanswered.is_empty()
    {
        let vaults: Vec<String> = asks
            .unanswered
            .iter()
            .map(|vault| format!("solana/{vault}"))
            .collect();
        out.push((
            "executor not answering".to_string(),
            format!("no total from the executor for: {}", vaults.join(", ")),
        ));
    }
    if ASKS.lock().is_ok_and(|asks| serving_none(&asks, now)) {
        out.push((
            "executor serves no vault".to_string(),
            "the executor answers 404 for every Solana vault it is asked about: check its URL"
                .to_string(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_claim_that_can_t_be_served_whole_is_tried_in_halves_down_to_a_share() {
        assert_eq!(
            halvings(100, None).collect::<Vec<_>>(),
            vec![50, 25, 12, 6, 3, 1]
        );
        assert_eq!(halvings(2, None).collect::<Vec<_>>(), vec![1]);
        // A claim of one share can only be served whole.
        assert_eq!(halvings(1, None).count(), 0);
        assert_eq!(halvings(0, None).count(), 0);
        // Never all of it, never none.
        assert!(halvings(u128::MAX, None).all(|part| part > 0 && part < u128::MAX));
        assert!(halvings(u128::MAX, None).count() <= 128);
    }

    #[test]
    fn bounded_partial_claim_probes_eventually_reach_one_share() {
        let shares = u128::MAX;
        let mut next = None;
        let mut all = Vec::new();
        for _ in 0..8 {
            let round: Vec<_> = halvings(shares, next)
                .take(CLAIM_PART_TRIES_A_ROUND as usize)
                .collect();
            assert!(round.len() <= CLAIM_PART_TRIES_A_ROUND as usize);
            next = round.last().map(|part| part / 2);
            all.extend(round);
            if next == Some(0) {
                break;
            }
        }
        assert_eq!(all, halvings(shares, None).collect::<Vec<_>>());
        assert_eq!(all.last(), Some(&1));
        assert_eq!(halvings(100, Some(25)).next(), Some(25));
        assert_eq!(halvings(10, Some(25)).next(), Some(5));
    }

    #[test]
    fn a_paid_part_requires_a_positive_cash_delta_and_fewer_shares() {
        assert!(!served(10, 20, None));
        assert!(served(10, 20, Some((ClaimState::Paid, 0, 21))));
        assert!(served(10, 20, Some((ClaimState::Pending, 5, 21))));
        for after in [
            (ClaimState::Pending, 5, 20),
            (ClaimState::Paid, 0, 20),
            (ClaimState::Pending, 5, 19),
            (ClaimState::Waiting, 10, 21),
            (ClaimState::Pending, 10, 21),
            (ClaimState::Paid, 10, 21),
            (ClaimState::Fallback, 0, 21),
        ] {
            assert!(!served(10, 20, Some(after)));
        }
        assert!(!served(0, 20, Some((ClaimState::Paid, 0, 21))));
    }

    #[test]
    fn a_whole_claim_keeps_ordinary_cash_queue_and_dust_completion() {
        assert!(settlement_progress(
            10,
            20,
            (ClaimState::Queued, 10, 20),
            true
        ));
        assert!(!settlement_progress(
            10,
            20,
            (ClaimState::Queued, 10, 20),
            false
        ));
        assert!(!settlement_progress(
            10,
            20,
            (ClaimState::Queued, 11, 20),
            true
        ));
        assert!(!settlement_progress(
            10,
            20,
            (ClaimState::Waiting, 10, 20),
            true
        ));
        assert!(settlement_progress(
            10,
            20,
            (ClaimState::Pending, 5, 21),
            false
        ));
        assert!(settlement_progress(10, 20, (ClaimState::Paid, 0, 20), true));
        assert!(settlement_progress(
            10,
            20,
            (ClaimState::Paid, 10, 20),
            true
        ));
        assert!(!settlement_progress(
            10,
            20,
            (ClaimState::Paid, 0, 19),
            true
        ));
    }

    #[test]
    fn a_stopped_station_is_unwound_again_in_its_slot_while_each_brings_something_home() {
        let dust = 10_000;
        // A cent home after the first: again.
        assert!(unwind_again(1, 10_000, dust));
        // Nothing worth it home: no more.
        assert!(!unwind_again(1, 0, dust));
        assert!(!unwind_again(1, UNWIND_WORTH_AT_LEAST - 1, 0));
        // At most a few in one slot, however much each brings.
        assert!(unwind_again(UNWINDS_A_SLOT - 1, u128::MAX, dust));
        assert!(!unwind_again(UNWINDS_A_SLOT, u128::MAX, dust));
    }

    #[tokio::test]
    async fn a_stopped_unwind_names_every_driver_that_goes_through_beside_the_others() {
        // Each goes alone; 0 and 1 together don't (too large for one
        // transaction, say); 3 never goes.
        let goes = |with: Vec<u8>| async move {
            !with.contains(&3) && !(with.contains(&0) && with.contains(&1))
        };
        assert_eq!(grown(4, goes).await, vec![0, 2]);
        assert_eq!(grown(0, goes).await, Vec::<u8>::new());
        assert_eq!(grown(3, |_| async { false }).await, Vec::<u8>::new());
        assert_eq!(grown(3, |_| async { true }).await, vec![0, 1, 2]);
    }

    use super::*;

    fn a_tank(
        balance: u64,
        settled: u64,
        as_of: u64,
        last_collection: u64,
        carried: u128,
    ) -> vp::state::Tank {
        vp::state::Tank {
            balance,
            keep_at: 0,
            usage_settled: settled,
            usage_as_of: as_of,
            last_collection,
            escape_requested_at: 0,
            cap_carried: carried,
            fill_pending: false,
            fill_request: 0,
        }
    }

    fn signed(total: u64, as_of: u64) -> SignedUsage {
        SignedUsage {
            total,
            as_of,
            signer: Pubkey::new_unique(),
            signature: [7; 64],
        }
    }

    #[test]
    fn a_signed_total_bills_only_what_the_vault_has_not_collected_and_was_not_sent() {
        let tank = a_tank(1_000, 50, 100, 100, 0);
        assert!(owes(&signed(60, 101), &tank, 0));
        assert!(
            !owes(&signed(50, 101), &tank, 0),
            "nothing more than collected"
        );
        assert!(
            !owes(&signed(60, 100), &tank, 0),
            "signed no later than collected"
        );
        assert!(!owes(&signed(60, 101), &tank, 101), "already sent");
    }

    #[test]
    fn a_keep_is_known_to_charge_nothing_from_an_empty_tank_or_past_the_grace() {
        assert_eq!(
            nothing_to_charge(&a_tank(0, 0, 0, 0, 0), None),
            Some(Billing::EmptyTank)
        );
        assert_eq!(
            nothing_to_charge(&a_tank(5, 0, 0, 500, 0), Some(500)),
            Some(Billing::Capped)
        );
        assert_eq!(
            nothing_to_charge(&a_tank(5, 0, 0, 500, 1), Some(500)),
            None,
            "cap carried past the grace"
        );
        assert_eq!(nothing_to_charge(&a_tank(5, 0, 0, 499, 0), Some(500)), None);
        assert_eq!(nothing_to_charge(&a_tank(5, 0, 0, 499, 0), None), None);
    }

    #[test]
    fn a_claim_left_waiting_is_tried_ever_less_often_and_any_other_next_slot() {
        let mut tried = None;
        let mut waits = Vec::new();
        for slot in [10, 11, 13, 17] {
            let next = next_try(tried, ClaimState::Waiting, slot);
            waits.push(next.1);
            tried = Some(next);
        }
        assert_eq!(waits, [1, 2, 4, 8]);
        assert_eq!(next_try(tried, ClaimState::Pending, 30), (31, 1));
        let mut tried = Some((0, MOST_SLOTS_BETWEEN_TRIES));
        tried = Some(next_try(tried, ClaimState::Waiting, 100));
        assert_eq!(tried.unwrap().1, MOST_SLOTS_BETWEEN_TRIES);
    }

    #[test]
    fn an_unwind_not_worth_sending_is_simulated_again_ever_less_often_up_to_five_minutes() {
        let mut check = None;
        let mut waits = Vec::new();
        for _ in 0..8 {
            let next = next_check(check, 1_000, 10);
            waits.push(next.1);
            check = Some(next);
        }
        assert_eq!(waits, [10, 20, 40, 80, 160, 300, 300, 300]);
        assert!(!worth_an_unwind(9_999, 0));
        assert!(worth_an_unwind(10_000, 82));
        assert!(
            !worth_an_unwind(20_000, 30_000),
            "less than the vault's dust"
        );
    }

    #[test]
    fn a_total_is_read_with_its_signature_in_base58_or_hex() {
        let signer = Pubkey::new_unique();
        let served = |total: serde_json::Value, signature: String| Served {
            total,
            as_of: 9,
            signature,
            signer: signer.to_string(),
        };
        let base58 = bs58::encode([3u8; 64]).into_string();
        let read = read_served(served(serde_json::json!("12"), base58)).unwrap();
        assert_eq!(
            (read.total, read.as_of, read.signer, read.signature),
            (12, 9, signer, [3; 64])
        );
        let hex = format!("0x{}", alloy::hex::encode([4u8; 64]));
        let read = read_served(served(serde_json::json!(13), hex)).unwrap();
        assert_eq!((read.total, read.signature), (13, [4; 64]));
        assert!(read_served(served(serde_json::json!("x"), "0x00".into())).is_err());
        assert!(
            read_served(served(serde_json::json!(1), "0x00".into())).is_err(),
            "a short signature"
        );
    }

    #[test]
    fn a_vault_whose_rounds_keep_failing_or_whose_send_does_not_go_out_is_in_the_health() {
        let vault = Pubkey::new_unique();
        let other = Pubkey::new_unique();
        let limited = Pubkey::new_unique();
        {
            let mut troubles = TROUBLES.lock().unwrap();
            troubles.unkept.insert(vault, MOST_VAULT_ROUNDS_FAILING);
            troubles.unkept.insert(other, 1);
            // A vault whose rounds the node's rate limit cuts short is said
            // as one that fails, with why.
            troubles.unkept.insert(limited, MOST_VAULT_ROUNDS_FAILING);
            troubles.limited.insert(limited);
            troubles
                .not_sent
                .insert((vault, "keep"), (Instant::now(), Said::default()));
        }
        let said = troubles(Instant::now());
        let line = |phrase: &str| {
            said.iter()
                .find(|(first, _)| first.contains(phrase))
                .cloned()
        };
        let (_, unkept) = line("vaults not kept").expect("the vault not kept is not said");
        assert!(
            unkept.contains(&vault.to_string()) && !unkept.contains(&other.to_string()),
            "{unkept}"
        );
        assert!(
            unkept.contains(&format!(
                "solana/{limited} ({MOST_VAULT_ROUNDS_FAILING} rounds in a row, the node's rate limit)"
            )),
            "{unkept}"
        );
        let (_, not_sent) = line("sends not going out").expect("the send is not said");
        assert!(not_sent.contains("keep"), "{not_sent}");
        // Once the same call goes out, it is no longer said.
        went_out(&(vault, "keep"));
        assert!(
            troubles(Instant::now())
                .iter()
                .all(|(_, detail)| !detail.contains(&format!("keep solana/{vault}")))
        );
        let mut troubles = TROUBLES.lock().unwrap();
        troubles.unkept.clear();
        troubles.limited.clear();
    }

    /// A vault priced a slot ago, paid this week, with every request priced.
    fn a_quiet_vault(now: u64) -> Vault {
        let mut vault = a_vault();
        vault.priced = true;
        vault.latest.time = now - 10;
        vault.valuation_count = 1;
        vault.last_valued_at = now - 10;
        vault.last_payment = now - DAY;
        vault.request_count = 3;
        vault.priced_through = 3;
        vault.tail_earliest = now - 10;
        vault.tail_start = 2;
        vault.previous_earliest = now - 20;
        vault.previous_start = 0;
        vault
    }

    #[test]
    fn a_solana_vault_is_valued_only_when_something_waits_for_it() {
        // Monday 13 November 2023, 00:00 UTC, and a day and a half on.
        let now = 1_699_833_600 + DAY + DAY / 2;
        let quiet = a_quiet_vault(now);
        assert_eq!(valuation_wanted(&quiet, now, false), None);

        // A request made this slot, to be priced at the next one.
        let mut asked = quiet.clone();
        asked.note_request(now + 10);
        assert_eq!(valuation_wanted(&asked, now, false), None);
        assert_eq!(
            valuation_wanted(&asked, now + 10, false),
            Some("a request waits for its price")
        );

        let mut payday = quiet.clone();
        payday.last_payment = now - 8 * DAY;
        assert_eq!(valuation_wanted(&payday, now, false), Some("payday"));

        let mut idle = quiet.clone();
        idle.latest.time = now - DAY;
        assert_eq!(
            valuation_wanted(&idle, now, false),
            Some("a day since the last priced valuation")
        );

        let mut never = quiet.clone();
        never.priced = false;
        assert_eq!(valuation_wanted(&never, now, false), Some("never priced"));

        // Halted after its last valuation: once for the halt, then each slot
        // only once the grace is over.
        let mut halted = quiet.clone();
        halted.halted_at = now - 5;
        halted.latest.gross = 5_000_000;
        assert_eq!(valuation_wanted(&halted, now, false), Some("its close"));
        halted.latest.time = now - 1;
        assert_eq!(valuation_wanted(&halted, now, false), None);
        assert_eq!(valuation_wanted(&halted, now, true), Some("its close"));
    }

    /// What a valuation sent with the clock at `now` is wanted for is judged
    /// in seconds, as the vault's own times are, not by the slot's number: a
    /// deposit due now, the payday and the heartbeat are each seen.
    #[test]
    fn a_solana_vault_sent_a_valuation_now_is_judged_at_its_slot_s_time() {
        // Monday 13 November 2023, 00:00 UTC, and a day and a half on: the
        // start of a slot.
        let now = 1_699_833_600 + DAY + DAY / 2;
        assert_eq!(valuation_wanted_now(&a_quiet_vault(now), now), None);

        let mut asked = a_quiet_vault(now);
        asked.note_request(now);
        assert_eq!(
            valuation_wanted_now(&asked, now + 3),
            Some("a request waits for its price")
        );

        let mut payday = a_quiet_vault(now);
        payday.last_payment = now - 8 * DAY;
        assert_eq!(valuation_wanted_now(&payday, now + 3), Some("payday"));

        let mut idle = a_quiet_vault(now);
        idle.latest.time = now - 2 * DAY;
        assert_eq!(
            valuation_wanted_now(&idle, now + 3),
            Some("a day since the last priced valuation")
        );
    }

    fn a_vault() -> Vault {
        let mut vault = Vault::try_deserialize_unchecked(&mut &[0u8; 8192][..]).unwrap();
        vault.state = VaultState::Open;
        vault.timers.freeze_after = 1_000;
        vault.timers.slot_interval = 10;
        vault
    }

    fn a_holder(units: u128, units_since: u64) -> Holder {
        Holder {
            vault: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            shares: 0,
            statement: [0; 32],
            units,
            unit_debt: 0,
            unit_credit: 0,
            units_since,
            pending: 0,
            payer: Pubkey::new_unique(),
            bump: 0,
        }
    }

    #[test]
    fn a_holder_s_units_are_tried_once_a_slot_and_ever_less_often_in_slots_and_rounds_after_serves_that_landed_and_failed()
     {
        // Slots a minute apart: an hour is sixty of them.
        let interval = 60;
        assert!(units_due(None, 5, 0));
        let mut tried = None;
        let mut waits = Vec::new();
        let (mut slot, mut round) = (10, 3);
        for _ in 0..9 {
            assert!(units_due(tried, slot, round));
            let next = next_units_try(tried, Some(Outcome::Reverted), slot, interval, round);
            assert!(
                !units_due(Some(next), slot, round),
                "tried again in the same slot"
            );
            // A clock that runs fast passes the wait in slots within a round:
            // the rounds hold the try back as long.
            assert!(
                !units_due(Some(next), next.next + 1_000, round + next.wait - 1),
                "tried again before as many rounds passed"
            );
            assert!(
                !units_due(Some(next), next.next - 1, round + 1_000),
                "tried again before as many slots passed"
            );
            assert!(units_due(Some(next), next.next, round + next.wait));
            waits.push(next.wait);
            slot = next.next;
            round = next.next_round;
            tried = Some(next);
        }
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        // A refusal costs nothing: next slot and any round, the wait kept
        // for the next that lands and fails.
        let refused = next_units_try(tried, Some(Outcome::Refused), 500, interval, 9);
        assert_eq!(
            refused,
            UnitsTry {
                next: 501,
                wait: 60,
                next_round: 9
            }
        );
        assert!(units_due(Some(refused), 501, 9));
        assert_eq!(
            next_units_try(None, Some(Outcome::NotSent), 7, interval, 2),
            UnitsTry {
                next: 8,
                wait: 1,
                next_round: 2
            }
        );
        let waited = UnitsTry {
            next: 0,
            wait: 4,
            next_round: 0,
        };
        assert_eq!(
            next_units_try(Some(waited), None, 7, interval, 2),
            UnitsTry {
                next: 8,
                wait: 4,
                next_round: 2
            }
        );
        assert_eq!(
            next_units_try(Some(waited), Some(Outcome::Reverted), 7, interval, 2),
            UnitsTry {
                next: 15,
                wait: 8,
                next_round: 10
            }
        );
        // Slots longer than an hour still wait one.
        assert_eq!(
            next_units_try(Some(waited), Some(Outcome::Reverted), 7, 2 * 60 * MINUTE, 2).wait,
            1
        );
    }

    #[test]
    fn a_queued_claim_is_not_tried_again_but_still_freezes_the_vault_a_freeze_period_after_booking()
    {
        assert!(claim_tried(ClaimState::Pending));
        assert!(claim_tried(ClaimState::Waiting));
        assert!(
            !claim_tried(ClaimState::Queued),
            "the next valuation pays it"
        );
        let vault = a_vault();
        // Booked at 100 with a freeze period of 1,000.
        for state in [ClaimState::Pending, ClaimState::Waiting, ClaimState::Queued] {
            assert!(
                !freeze_for_claim_due(state, 100, &vault, 1_099),
                "{state:?} too early"
            );
            assert!(freeze_for_claim_due(state, 100, &vault, 1_100), "{state:?}");
        }
        for state in [ClaimState::Paid, ClaimState::Fallback] {
            assert!(
                !freeze_for_claim_due(state, 100, &vault, 5_000),
                "{state:?}"
            );
        }
        let mut halted = a_vault();
        halted.halted_at = 50;
        assert!(
            !freeze_for_claim_due(ClaimState::Queued, 100, &halted, 5_000),
            "halted"
        );
        let mut closed = a_vault();
        closed.state = VaultState::Closed;
        assert!(
            !freeze_for_claim_due(ClaimState::Queued, 100, &closed, 5_000),
            "closed"
        );
        let mut owed = a_vault();
        owed.priced = true;
        owed.priced_through = 3;
        owed.request_cursor = 1;
        assert!(
            !freeze_for_claim_due(ClaimState::Queued, 100, &owed, 5_000),
            "a valuation's settlement is owed"
        );
    }

    #[test]
    fn a_vault_is_frozen_for_units_only_once_they_waited_a_freeze_period_on_a_live_vault_owing_nothing()
     {
        let vault = a_vault();
        let holder = a_holder(5, 100);
        assert!(!freeze_for_units_due(&holder, &vault, 1_099), "too early");
        assert!(freeze_for_units_due(&holder, &vault, 1_100));
        assert!(
            !freeze_for_units_due(&a_holder(0, 100), &vault, 5_000),
            "no units left"
        );
        let mut halted = a_vault();
        halted.halted_at = 50;
        assert!(!freeze_for_units_due(&holder, &halted, 5_000), "halted");
        let mut closed = a_vault();
        closed.state = VaultState::Closed;
        assert!(!freeze_for_units_due(&holder, &closed, 5_000), "closed");
        let mut winding = a_vault();
        winding.state = VaultState::WindingDown;
        assert!(freeze_for_units_due(&holder, &winding, 5_000));
        let mut owed = a_vault();
        owed.priced = true;
        owed.priced_through = 3;
        owed.request_cursor = 1;
        assert!(
            !freeze_for_units_due(&holder, &owed, 5_000),
            "a valuation's settlement is owed"
        );
    }

    #[test]
    fn the_restricted_list_is_read_again_only_for_an_open_vault_not_halted_and_sent_only_if_it_lengthens()
     {
        let vault = a_vault();
        assert!(refresh_wanted(&vault));
        for state in [
            VaultState::WindingDown,
            VaultState::Frozen,
            VaultState::Closed,
        ] {
            let mut other = a_vault();
            other.state = state;
            assert!(!refresh_wanted(&other), "{state:?}");
        }
        let mut halted = a_vault();
        halted.halted_at = 1;
        assert!(!refresh_wanted(&halted));

        let mut longer = a_vault();
        longer.restricted_hash = [9; 32];
        assert!(lengthens(&vault, Some(&longer)));
        assert!(!lengthens(&vault, Some(&a_vault())), "the same list");
        assert!(!lengthens(&vault, None), "nothing read back");
    }

    #[test]
    fn an_executor_answering_404_for_every_wanted_vault_is_said_to_serve_none_until_one_is_served_or_none_is_wanted()
     {
        let (first, second) = (Pubkey::new_unique(), Pubkey::new_unique());
        let later = || Instant::now() + crate::serving_none_for();
        let mut asks = Asks::default();
        asks.note_serving();
        assert!(!serving_none(&asks, later()), "asked about no vault");

        let sent = Instant::now();
        asks.latest.insert(first, (Asked::NotServed, sent));
        asks.latest.insert(second, (Asked::NotServed, sent));
        asks.note_serving();
        assert!(!serving_none(&asks, Instant::now()), "said at once");
        assert!(serving_none(&asks, later()));

        asks.latest
            .insert(second, (Asked::Total(signed(1, 1)), Instant::now()));
        asks.note_serving();
        assert!(
            !serving_none(&asks, later()),
            "a vault is served, yet the executor is said to serve none"
        );

        // A vault no longer wanting a total doesn't count: the other's 404
        // is then every wanted vault's.
        asks.latest.insert(second, (Asked::NotServed, sent));
        asks.not_wanted.insert(second, Instant::now());
        asks.note_serving();
        assert!(serving_none(&asks, later()));
        asks.not_wanted.insert(first, Instant::now());
        asks.note_serving();
        assert!(
            !serving_none(&asks, later()),
            "no vault wants a total, yet the executor is said to serve none"
        );
    }

    #[test]
    fn an_executor_serving_no_vault_is_in_the_health() {
        ASKS.lock().unwrap().serving_none_since = Some(Instant::now());
        let said = troubles(Instant::now() + crate::serving_none_for());
        ASKS.lock().unwrap().serving_none_since = None;
        assert!(
            said.iter()
                .any(|(first, _)| first == "executor serves no vault"),
            "{said:?}"
        );
        assert!(
            troubles(Instant::now() + crate::serving_none_for())
                .iter()
                .all(|(first, _)| first != "executor serves no vault")
        );
    }

    #[test]
    fn a_failed_step_is_kept_until_a_round_of_its_vault_finishes_without_it() {
        let vault = Pubkey::new_unique();
        let kept = || STEPS_FAILED.lock().unwrap().contains_key(&(vault, "units"));
        carry_on(&vault, "units", Err(anyhow::anyhow!("the node is down")));
        assert!(kept());
        steps_worked(&vault, true);
        assert!(kept(), "it failed in the round that just finished");
        steps_worked(&vault, false);
        assert!(kept(), "a round that did not finish ends nothing");
        steps_worked(&vault, true);
        assert!(!kept(), "a round finished without it, yet it is kept");
    }

    #[test]
    fn a_program_error_is_read_as_the_vault_refusing_or_as_stale() {
        let failed =
            |custom: Option<u32>, signature: Option<String>| -> Result<chain::solana::Landed> {
                Err(TransactionFailed {
                    signature,
                    err: serde_json::Value::Null,
                    custom,
                    logs: vec![],
                }
                .into())
            };
        let stale = vault_error(vp::error::VaultError::Stale);
        assert_eq!(Outcome::of(&failed(Some(stale), None)), Outcome::Stale);
        assert_eq!(
            Outcome::of(&failed(Some(stale + 1), None)),
            Outcome::Refused
        );
        assert_eq!(
            Outcome::of(&failed(Some(stale + 1), Some("s".into()))),
            Outcome::Reverted
        );
        assert_eq!(
            Outcome::of(&Err(anyhow::anyhow!("the node is down"))),
            Outcome::NotSent
        );
    }
}
