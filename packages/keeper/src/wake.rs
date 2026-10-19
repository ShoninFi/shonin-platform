//! When the keeper next looks at a chain.
//!
//! Between looks the keeper reads nothing of the chain. It looks again at
//! the earliest of:
//!
//! - each running vault's next slot (open, winding down, frozen or
//!   halted): its valuation window opens a block before the
//!   slot (a valuation sent then lands inside it), and the slot itself
//!   begins (a claim's try, a holder's serve and an unwind are made at most
//!   once a slot). Inside a window it looks again at the active pace while
//!   another look could still land a valuation in it;
//! - each due time a vault's state or the keeper's memory gives, in chain
//!   time ([`dues`]): a deadline, a calendar point, the end of a back-off or
//!   a hold. A due time whose block hasn't appeared when the keeper looks is
//!   looked for again a second later (or at the active pace, if quicker);
//! - holds measured on the keeper's own clock;
//! - and at most `keeper_notice_ms` after the last look, so whatever someone
//!   else did is seen within it.
//!
//! An executor's answer, and a send of the keeper's own, are looked at
//! straight away (see the chain's loop). Whatever can't be computed (a view
//! the look didn't hold) sends the chain to the active pace.

use std::time::{Duration, Instant};

use alloy::primitives::{Address, U256};
use chain::bindings::station::{IStation, IStationVault, Stage};
use chain::bindings::vault::{ClaimState, IVault, ValuationStatus, VaultState};

use crate::observed::Observed;
use crate::{DAY, LAST_ORDERS, MINUTE, Memory, own_clock_hold};

/// What the next look is planned from.
#[derive(Debug, Default)]
pub struct Dues {
    /// The chain time of the look just made (its head's timestamp).
    pub head: u64,
    /// Each open or winding-down vault's slot interval and window, once.
    pub slots: Vec<(u64, u64)>,
    /// Due times in chain time, and why.
    pub chain: Vec<(u64, &'static str)>,
    /// Due times on the keeper's own clock, and why.
    pub wall: Vec<(Instant, &'static str)>,
    /// Something the plan needs couldn't be read from the look: the next
    /// look is at the active pace.
    pub unknown: Option<&'static str>,
}

impl Dues {
    pub(crate) fn at(&mut self, time: u64, why: &'static str) {
        if time > self.head {
            self.chain.push((time, why));
        }
    }

    fn slot(&mut self, interval: u64, window: u64) {
        if !self.slots.contains(&(interval, window)) {
            self.slots.push((interval, window));
        }
    }
}

/// The next look, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Next {
    pub at: Instant,
    pub why: &'static str,
}

/// When to look next, from `dues`, the look made at `looked` (on the
/// keeper's clock), the chain's block interval in seconds, the active pace
/// and the notice bound. `to_wall` turns a chain time into the moment the
/// chain's head is expected to reach it, when the chain's clock is bounded.
pub fn next(
    dues: &Dues,
    block_interval: u64,
    active: Duration,
    notice: Duration,
    looked: Instant,
    now: Instant,
    to_wall: impl Fn(u64) -> Option<Instant>,
) -> Next {
    let soon = now + active;
    // No look sooner than a second after the last (a chain's clock counts
    // whole seconds), unless the active pace is quicker.
    let floor = looked + active.min(Duration::from_secs(1));
    let mut best = Next {
        at: looked + notice,
        why: "the notice bound",
    };
    let mut consider = |at: Instant, why: &'static str| {
        if at < best.at {
            best = Next { at, why };
        }
    };
    if let Some(why) = dues.unknown {
        consider(soon, why);
    }
    let mut consider = |at: Instant, why: &'static str| consider(at.max(floor), why);
    // A chain time is looked for when the head is expected to reach it, and
    // at the active pace once that moment has passed without it.
    let when = |time: u64, why: &'static str, consider: &mut dyn FnMut(Instant, &'static str)| {
        if time <= dues.head {
            return;
        }
        match to_wall(time) {
            Some(at) if at > now => consider(at, why),
            // Its moment has passed and its block isn't out yet: looked for
            // again a second on (the floor), so the block it lands in is
            // not missed by a whole active round.
            Some(_) => consider(now + active.min(Duration::from_secs(1)), why),
            None => consider(soon, why),
        }
    };
    for (time, why) in &dues.chain {
        when(*time, why, &mut consider);
    }
    for (at, why) in &dues.wall {
        if *at > now {
            consider(*at, why);
        }
    }
    let block_interval = block_interval.max(1);
    let active_s = active.as_secs().max(1);
    for &(interval, window) in &dues.slots {
        let interval = interval.max(1);
        let current = dues.head - dues.head % interval;
        for slot in [current, current + interval, current + 2 * interval] {
            // The slot begins: what is done once a slot is done then.
            when(slot, "a slot begins", &mut consider);
            // Its valuation window, as the keeper judges a send landing a
            // block after the head it was decided at.
            let open = slot.saturating_sub(block_interval);
            let close = (slot + window).saturating_sub(block_interval);
            if dues.head >= close {
                continue;
            }
            if dues.head < open {
                when(open, "a slot's valuation window opens", &mut consider);
                break;
            }
            // Inside the window: looked at again while another look could
            // still land a valuation in it; otherwise the next slot's.
            if dues.head + active_s <= close {
                consider(soon, "inside a slot's valuation window");
                break;
            }
        }
    }
    best
}

/// Add what one vault is next due for, from what the wake read of it and
/// the keeper's memory of it, without reading anything more. Mirrors the
/// conditions `keep_one` acts on: each due time is the first chain time at
/// which one of them can change.
pub fn dues(client: &Observed, vault: Address, memory: &Memory, out: &mut Dues) {
    let head = out.head;
    // Paces and holds the keeper keeps whatever the vault's state.
    if let Some(again) = memory.ask_again {
        out.at(again.next, "an ask made again");
    }
    if let Some(again) = memory.keep_again {
        out.at(again.next, "a bill's keep sent again");
    }
    for again in memory.serve_again.values() {
        out.at(again.next, "a holder's serve sent again");
    }
    for (since, why) in [
        (memory.close_held_since, "a close's hold ends"),
        (memory.last_try_since, "a bill's last try ends"),
    ] {
        if let Some((chain, wall)) = since {
            out.at(chain.saturating_add(LAST_ORDERS), why);
            out.wall.push((wall + own_clock_hold(), why));
        }
    }
    let Some(Ok(state)) = client.peek(vault, IVault::stateCall {}) else {
        out.unknown = Some("a vault's state was not read");
        return;
    };
    match state {
        VaultState::None => {}
        VaultState::Activating => {
            match (
                client.peek(vault, IVault::createdAtCall {}),
                client.peek(vault, IVault::timersCall {}),
            ) {
                (Some(Ok(created)), Some(Ok(timers))) => {
                    out.at(
                        created.saturating_add(timers.activationWindow),
                        "an unopened vault may be cancelled",
                    );
                }
                _ => out.unknown = Some("an unopened vault's window was not read"),
            }
        }
        VaultState::Closed | VaultState::Cancelled => {
            if let Some(quiet) = memory.quiet_since
                && head < quiet.saturating_add(DAY)
            {
                out.at(quiet.saturating_add(DAY), "a quiet vault's daily look");
                return;
            }
            if let Some(asked) = memory.bill_asked_at {
                out.at(asked.saturating_add(MINUTE), "a closed vault's ask");
            }
            let grace_end = halt_end(client, vault, out);
            if let Some((seen, wall)) = memory.closed_seen_at {
                out.at(
                    seen.max(grace_end.unwrap_or(0)).saturating_add(LAST_ORDERS),
                    "a closed vault's last bill",
                );
                out.wall
                    .push((wall + own_clock_hold(), "a closed vault's last bill"));
            }
            if let Some(end) = grace_end {
                out.at(end, "a closed vault's station may be stopped");
            }
            // A stopped station's unwind is simulated at most once a slot.
            if (memory.unwind_checks.is_some() || memory.unwound_in.is_some())
                && let Some(Ok(timers)) = client.peek(vault, IVault::timersCall {})
            {
                let interval = timers.slotInterval.max(1);
                out.at(head - head % interval + interval, "a closed vault's unwind");
            }
            if let Some(next) = memory.delayed.next_unwind() {
                out.at(next, "a delayed station's next unwind");
            }
        }
        VaultState::Open | VaultState::WindingDown | VaultState::Frozen | VaultState::Killed => {
            let Some(Ok(timers)) = client.peek(vault, IVault::timersCall {}) else {
                out.unknown = Some("a vault's timers were not read");
                return;
            };
            out.slot(timers.slotInterval, timers.slotWindow);
            if let Some(daily) = memory.daily_at {
                out.at(daily.saturating_add(DAY), "the daily round");
            }
            let halted = match client.peek(vault, IStationVault::haltedAtCall {}) {
                Some(Ok(halted)) => halted,
                _ => {
                    out.unknown = Some("whether a vault halted was not read");
                    0
                }
            };
            if halted != 0
                && let Some(end) = halt_end(client, vault, out)
            {
                out.at(end, "a halt's grace ends");
            }
            if let Some(Ok(tank)) = client.peek(vault, IVault::tankCall {})
                && tank.escapeRequestedAt != 0
                && tank.escapeRequestedAt != memory.escape_collected
                && let Some(asked) = memory.bill_asked_at
            {
                out.at(asked.saturating_add(MINUTE), "a tank withdrawal's ask");
            }
            // The newest valuation, once its other chains' deadline passes.
            match client.peek(vault, IVault::valuationCountCall {}) {
                Some(Ok(count)) if !count.is_zero() => match client.peek(
                    vault,
                    IVault::valuationAtCall {
                        index: count - U256::from(1u64),
                    },
                ) {
                    Some(Ok(latest)) => {
                        if matches!(latest.status, ValuationStatus::Open) {
                            out.at(
                                latest.time.saturating_add(timers.postDeadline) + 1,
                                "an open valuation's deadline",
                            );
                        }
                    }
                    _ => out.unknown = Some("a vault's newest valuation was not read"),
                },
                Some(Ok(_)) => {}
                _ => out.unknown = Some("a vault's valuations were not read"),
            }
            if halted == 0 {
                claim_freezes(client, vault, memory, timers.freezeAfter, out);
                for &holder in &memory.holders {
                    if let Some(Ok(units)) = client.peek(vault, IVault::unitsOfCall { holder })
                        && !units.units.is_zero()
                        && let Some(Ok(since)) =
                            client.peek(vault, IVault::unitsSinceCall { holder })
                    {
                        out.at(
                            since.saturating_add(timers.freezeAfter),
                            "a holder's units may be frozen for",
                        );
                    }
                }
            }
            if matches!(state, VaultState::WindingDown)
                && let Some(retired) = memory.retired_at
            {
                out.at(
                    retired.saturating_add(timers.windDown),
                    "a retired vault may be evicted",
                );
            }
            if let Some(next) = memory.delayed.next_unwind() {
                out.at(next, "a delayed station's next unwind");
            }
            if let Some(Ok(station)) = client.peek(vault, IVault::homeStationCall {})
                && let Some(Ok(stage)) = client.peek(station, IStation::stageCall {})
                && stage.stagedAt != 0
            {
                staged_close(&stage, out);
            }
        }
        _ => {}
    }
}

/// A staged close's due times, as the station's `settleClose` takes them: it
/// may first be proved once its settle delay has passed (wholly closed); a
/// partly filled one once its window, counted from the stage, is over too;
/// unswept, it lapses from its deadline; and once swept, the part that
/// landed is delivered a window after the ask (a swept close never lapses).
fn staged_close(stage: &Stage, out: &mut Dues) {
    let placed = stage.stagedAt;
    let provable = placed.saturating_add(stage.delay);
    out.at(provable, "a staged close may be proved");
    out.at(
        placed.saturating_add(stage.window).max(provable),
        "a staged close's proving window ends",
    );
    if stage.sweptAt == 0 {
        out.at(stage.provedBy, "a staged close's proving deadline passes");
    } else {
        out.at(
            stage.sweptAt.saturating_add(stage.window),
            "a staged close's delivery window ends",
        );
    }
}

/// When the vault's halt's grace ends, if it has halted, from the wake's
/// reads.
fn halt_end(client: &Observed, vault: Address, out: &mut Dues) -> Option<u64> {
    match client.peek(vault, IStationVault::haltedAtCall {}) {
        Some(Ok(0)) => None,
        Some(Ok(halted)) => match client.peek(vault, IStationVault::haltGraceCall {}) {
            Some(Ok(grace)) => Some(halted.saturating_add(grace)),
            _ => {
                out.unknown = Some("a halt's grace was not read");
                None
            }
        },
        _ => None,
    }
}

/// Each unpaid claim may freeze the vault a freeze period after it was
/// booked.
fn claim_freezes(
    client: &Observed,
    vault: Address,
    memory: &Memory,
    freeze_after: u64,
    out: &mut Dues,
) {
    let count: u64 = match client.peek(vault, IVault::claimCountCall {}) {
        Some(Ok(count)) => count.try_into().unwrap_or(u64::MAX),
        _ => {
            out.unknown = Some("a vault's claims were not read");
            return;
        }
    };
    for id in memory.claims_from.max(1)..=count {
        match client.peek(
            vault,
            IVault::claimAtCall {
                claimId: U256::from(id),
            },
        ) {
            Some(Ok(claim)) => {
                if matches!(
                    claim.state,
                    ClaimState::Pending
                        | ClaimState::Waiting
                        | ClaimState::Queued
                        | ClaimState::Closing
                ) {
                    out.at(
                        claim.bookedAt.saturating_add(freeze_after),
                        "an unpaid claim may freeze the vault",
                    );
                }
            }
            _ => {
                out.unknown = Some("a vault's claim was not read");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIVE: Duration = Duration::from_secs(2);
    const NOTICE: Duration = Duration::from_secs(5);

    /// A chain whose head reaches chain time `t` at `start + (t - head)`
    /// seconds.
    fn clock(start: Instant, head: u64) -> impl Fn(u64) -> Option<Instant> {
        move |time: u64| {
            Some(if time >= head {
                start + Duration::from_secs(time - head)
            } else {
                start - Duration::from_secs(head - time)
            })
        }
    }

    fn plan(dues: &Dues, block_interval: u64) -> (Duration, &'static str) {
        let start = Instant::now();
        let next = next(
            dues,
            block_interval,
            ACTIVE,
            NOTICE,
            start,
            start,
            clock(start, dues.head),
        );
        (next.at.saturating_duration_since(start), next.why)
    }

    fn within(found: Duration, wanted: u64) -> bool {
        found.abs_diff(Duration::from_secs(wanted)) < Duration::from_millis(50)
    }

    #[test]
    fn with_nothing_due_the_next_look_is_the_notice_bound() {
        let dues = Dues {
            head: 1_000_003,
            ..Dues::default()
        };
        let (after, why) = plan(&dues, 2);
        assert!(within(after, 5), "{after:?}");
        assert_eq!(why, "the notice bound");
    }

    #[test]
    fn a_slot_s_window_is_looked_at_a_block_before_the_slot() {
        // Slots every 10 s, a 3 s window, 2 s blocks: a valuation decided at
        // the head of t - 2 lands at t, the window's first moment.
        let dues = Dues {
            head: 1_000_003,
            slots: vec![(10, 3)],
            ..Dues::default()
        };
        // The window of the slot at 1_000_010 opens at 1_000_008, 5 s on;
        // the notice bound comes first.
        let (after, _) = plan(&dues, 2);
        assert!(within(after, 5), "{after:?}");
        let dues = Dues {
            head: 1_000_006,
            slots: vec![(10, 3)],
            ..Dues::default()
        };
        let (after, why) = plan(&dues, 2);
        assert!(within(after, 2), "{after:?}");
        assert_eq!(why, "a slot's valuation window opens");
    }

    #[test]
    fn inside_a_window_another_look_is_made_only_while_it_could_still_land_there() {
        let dues = |head| Dues {
            head,
            slots: vec![(10, 3)],
            ..Dues::default()
        };
        // At t - 2 another look at the active pace (t) still lands at t + 2.
        let (after, why) = plan(&dues(1_000_008), 2);
        assert!(within(after, 2), "{after:?}");
        assert!(why == "inside a slot's valuation window" || why == "a slot begins");
        // At t no look 2 s later could land inside the window (t + 4 > t + 3):
        // the next slot's window is next, past the notice bound.
        let (after, why) = plan(&dues(1_000_010), 2);
        assert!(within(after, 5), "{after:?}");
        assert_eq!(why, "the notice bound");
    }

    #[test]
    fn with_a_long_notice_bound_the_next_slot_s_window_is_still_looked_at() {
        // At the current slot's start no look could still land in its
        // window: the next slot's window, 8 s on, is the next look, not a
        // notice bound of a minute.
        let dues = Dues {
            head: 1_000_010,
            slots: vec![(10, 3)],
            ..Dues::default()
        };
        let start = Instant::now();
        let next = next(
            &dues,
            2,
            ACTIVE,
            Duration::from_secs(60),
            start,
            start,
            clock(start, dues.head),
        );
        assert!(within(next.at - start, 8), "{:?}", next.at - start);
        assert_eq!(next.why, "a slot's valuation window opens");
    }

    #[test]
    fn a_slot_s_start_is_looked_at_even_when_its_window_closes_before_it() {
        // A 1 s window on 2 s blocks closes a second before the slot.
        let dues = Dues {
            head: 1_000_009,
            slots: vec![(10, 1)],
            ..Dues::default()
        };
        let (after, why) = plan(&dues, 2);
        assert!(within(after, 1), "{after:?}");
        assert_eq!(why, "a slot begins");
    }

    #[test]
    fn a_due_time_is_looked_at_when_the_head_should_reach_it_and_again_a_second_later() {
        let mut dues = Dues {
            head: 1_000_000,
            ..Dues::default()
        };
        dues.at(1_000_003, "a deadline");
        let (after, why) = plan(&dues, 2);
        assert!(within(after, 3), "{after:?}");
        assert_eq!(why, "a deadline");
        // Its moment has passed on the keeper's clock and the head hasn't
        // reached it (a block not out yet): looked for at the active pace.
        let start = Instant::now();
        let next = next(
            &dues,
            2,
            ACTIVE,
            NOTICE,
            start,
            start,
            clock(start - Duration::from_secs(10), dues.head),
        );
        assert!(within(next.at - start, 1));
        assert_eq!(next.why, "a deadline");
    }

    #[test]
    fn a_due_time_the_head_has_reached_was_acted_on_and_plans_nothing() {
        let mut dues = Dues {
            head: 1_000_000,
            ..Dues::default()
        };
        dues.at(1_000_000, "a deadline");
        dues.at(999_000, "an older deadline");
        assert!(dues.chain.is_empty());
        let (after, _) = plan(&dues, 2);
        assert!(within(after, 5));
    }

    #[test]
    fn a_hold_on_the_keeper_s_clock_wakes_when_it_ends() {
        let start = Instant::now();
        let dues = Dues {
            head: 1_000_000,
            wall: vec![
                (start + Duration::from_secs(1), "a hold"),
                (start - Duration::from_secs(1), "a hold already over"),
            ],
            ..Dues::default()
        };
        let next = next(
            &dues,
            2,
            ACTIVE,
            NOTICE,
            start,
            start,
            clock(start, dues.head),
        );
        assert!(within(next.at - start, 1));
        assert_eq!(next.why, "a hold");
    }

    #[test]
    fn what_cannot_be_computed_sends_the_chain_to_the_active_pace() {
        let dues = Dues {
            head: 1_000_000,
            unknown: Some("not read"),
            ..Dues::default()
        };
        let (after, why) = plan(&dues, 2);
        assert!(within(after, 2));
        assert_eq!(why, "not read");
    }

    #[test]
    fn a_chain_whose_clock_is_unbounded_has_due_times_looked_at_the_active_pace() {
        let mut dues = Dues {
            head: 1_000_000,
            ..Dues::default()
        };
        dues.at(1_000_004, "a deadline");
        let start = Instant::now();
        let next = next(&dues, 2, ACTIVE, NOTICE, start, start, |_| None);
        assert!(within(next.at - start, 2));
    }

    fn stage(staged_at: u64, swept_at: u64) -> Stage {
        Stage {
            fraction: U256::from(1u64),
            owed: U256::ZERO,
            estimate: U256::ZERO,
            fetched: U256::ZERO,
            cost: U256::ZERO,
            part: U256::ZERO,
            arrivedAt: U256::ZERO,
            stagedAt: staged_at,
            stagedBlock: 1,
            sweptAt: swept_at,
            sweptBlock: u64::from(swept_at != 0),
            window: 600,
            delay: 6,
            provedBy: staged_at + 2 * 600,
            forVault: false,
        }
    }

    #[test]
    fn a_staged_close_is_looked_at_when_it_may_be_proved_when_its_window_ends_and_at_its_deadline()
    {
        let mut dues = Dues {
            head: 1_000_000,
            ..Dues::default()
        };
        staged_close(&stage(1_000_000, 0), &mut dues);
        // A partly filled close is proved once the window from the stage is
        // over, with no settle delay on top.
        assert_eq!(
            dues.chain,
            vec![
                (1_000_006, "a staged close may be proved"),
                (1_000_600, "a staged close's proving window ends"),
                (1_001_200, "a staged close's proving deadline passes"),
            ]
        );
    }

    #[test]
    fn a_staged_close_s_window_never_ends_before_its_settle_delay() {
        let mut dues = Dues {
            head: 1_000_000,
            ..Dues::default()
        };
        let mut staged = stage(1_000_000, 0);
        staged.window = 4;
        staged.provedBy = 1_000_008;
        staged_close(&staged, &mut dues);
        assert_eq!(
            dues.chain,
            vec![
                (1_000_006, "a staged close may be proved"),
                (1_000_006, "a staged close's proving window ends"),
                (1_000_008, "a staged close's proving deadline passes"),
            ]
        );
    }

    #[test]
    fn a_swept_staged_close_is_looked_at_when_its_delivery_window_ends() {
        let mut dues = Dues {
            head: 1_000_700,
            ..Dues::default()
        };
        staged_close(&stage(1_000_000, 1_000_650), &mut dues);
        // Its proving times have passed, and a swept close never lapses:
        // only the delivery window's end is ahead.
        assert_eq!(
            dues.chain,
            vec![(1_001_250, "a staged close's delivery window ends")]
        );
    }
}
