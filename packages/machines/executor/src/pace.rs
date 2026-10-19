//! Each agent held to the fuel a second it declared.
//!
//! An agent's fuel allowance is what its tank pays for, and the share of the
//! machine its reservation stands for: it is a rate, so it is held as one.
//! Each vault's agent draws on a budget that fills at its declared fuel a
//! second, and holds at most a [`WINDOW`] of it: within the window a burst is
//! free, and an agent idle for longer has no more saved up than that.
//!
//! The budget is charged with the fuel the agent burnt, read from its store
//! at every check of the engine's clock ([`CHECK_EVERY`]), and also when a
//! run of the agent ends. The clock's thread may be kept waiting on a busy
//! machine, so a check is also brought forward whenever the agent has burnt
//! another [`crate::runner::YIELD_FUEL`] and its budget is dry. When the
//! budget runs dry the agent's task is parked, an async wait between two
//! instructions, until the budget has filled again; other agents and the
//! host carry on meanwhile. A check sees
//! only what the agent burnt since the last one, so an agent can burn past
//! what is left: that is owed, and repaid out of what fills before it runs
//! again. So that little is owed, an agent is parked before it starts on a
//! slice of work it can't pay for: it runs only when the budget holds what it
//! burnt in its latest slice, twice over (at most the whole window).
//!
//! The store's count of fuel is brought up to date at every call, return
//! and yield, so a loop that makes no calls shows what it burnt only every
//! [`crate::runner::YIELD_FUEL`]: an agent can be that much ahead of what it
//! was charged, which it is charged at the next showing. What it burnt after
//! the last showing of a run that ends is never seen: at most that slice a
//! run.
//!
//! The budget belongs to the vault, not to one run of its agent: a restart
//! doesn't fill it, and what is owed is still owed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::time::Instant;

/// How much of its rate an agent may save up, and so burst through at once.
pub const WINDOW: Duration = Duration::from_secs(1);

/// How often the engine's clock ticks, and so how often a running agent's
/// fuel is read and charged. The yields the clock forces stay every
/// [`crate::runner`]'s yield interval; most ticks only charge the budget.
pub const CHECK_EVERY: Duration = Duration::from_millis(1);

/// The longest a parked agent sleeps before looking at its budget again, so
/// a change to the rate (or a clock that jumped) never leaves it parked for
/// long past when it could run.
const NAP_AT_MOST: Duration = Duration::from_secs(1);

const NANOS: u128 = 1_000_000_000;

/// One vault's fuel budget.
#[derive(Debug)]
pub struct Budget {
    bucket: Mutex<Bucket>,
}

#[derive(Clone, Debug)]
struct Bucket {
    /// Fuel a second it fills at. Never zero.
    rate: u64,
    /// Fuel the agent may burn now, at most a window of the rate.
    level: u64,
    /// Fuel burnt past what was there, repaid before anything fills.
    owed: u64,
    /// A part of a unit of fuel filled but not yet whole, in billionths, so
    /// a budget charged often loses nothing to rounding.
    part: u128,
    /// When it was last filled.
    at: Instant,
    /// What the agent burnt in its latest slice between two checks: what it
    /// is expected to burn in its next.
    slice: u64,
    /// All the fuel charged to it.
    used: u64,
}

impl Bucket {
    /// The most it holds: a window of the rate.
    fn most(&self) -> u64 {
        let most = u128::from(self.rate) * WINDOW.as_nanos() / NANOS;
        u64::try_from(most).unwrap_or(u64::MAX).max(1)
    }

    /// Fill it for the time since it was last filled, what is owed first.
    fn fill(&mut self, now: Instant) {
        if now <= self.at {
            return;
        }
        let nanos = now.duration_since(self.at).as_nanos();
        self.at = now;
        let filled = u128::from(self.rate)
            .saturating_mul(nanos)
            .saturating_add(self.part);
        self.part = filled % NANOS;
        let mut whole = u64::try_from(filled / NANOS).unwrap_or(u64::MAX);
        let repaid = whole.min(self.owed);
        self.owed -= repaid;
        whole -= repaid;
        self.level = self.level.saturating_add(whole).min(self.most());
        if self.level == self.most() {
            self.part = 0;
        }
    }

    /// Charge `used`, past what is there as owed.
    fn charge(&mut self, used: u64) {
        self.used = self.used.saturating_add(used);
        if used <= self.level {
            self.level -= used;
        } else {
            self.owed = self.owed.saturating_add(used - self.level);
            self.level = 0;
        }
    }

    /// What it must hold for the agent to run: nothing owed, and the next
    /// slice paid for ahead, at twice the latest (never more than it can
    /// hold, never nothing).
    fn needed(&self) -> u64 {
        self.slice.saturating_mul(2).clamp(1, self.most())
    }

    /// How long until the agent may run again: none if it may now.
    fn wait(&self) -> Duration {
        let short = self
            .owed
            .saturating_add(self.needed())
            .saturating_sub(self.level);
        if short == 0 {
            return Duration::ZERO;
        }
        let nanos = (u128::from(short) * NANOS)
            .saturating_sub(self.part)
            .div_ceil(u128::from(self.rate));
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

impl Budget {
    /// An empty budget filling at `rate` fuel a second: an agent starts with
    /// nothing saved up.
    pub fn new(rate: u64, now: Instant) -> Self {
        Self {
            bucket: Mutex::new(Bucket {
                rate: rate.max(1),
                level: 0,
                owed: 0,
                part: 0,
                at: now,
                slice: 0,
                used: 0,
            }),
        }
    }

    fn bucket(&self) -> std::sync::MutexGuard<'_, Bucket> {
        self.bucket
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Fill at `rate` from `now` on: what filled until now filled at the
    /// rate before, and a lower rate's window keeps what fits.
    pub fn set_rate(&self, rate: u64, now: Instant) {
        let mut bucket = self.bucket();
        bucket.fill(now);
        bucket.rate = rate.max(1);
        bucket.level = bucket.level.min(bucket.most());
    }

    /// Charge the fuel the agent burnt in its latest slice, and say whether
    /// it must be parked before it runs on.
    pub fn spend(&self, used: u64, now: Instant) -> bool {
        let mut bucket = self.bucket();
        bucket.fill(now);
        bucket.charge(used);
        bucket.slice = used;
        !bucket.wait().is_zero()
    }

    /// Whether, were the agent charged `used` now, it would be parked: what
    /// [`Budget::spend`] would say, charging nothing.
    pub fn dry_after(&self, used: u64, now: Instant) -> bool {
        let mut bucket = self.bucket();
        bucket.fill(now);
        let mut after = bucket.clone();
        after.charge(used);
        after.slice = used;
        !after.wait().is_zero()
    }

    /// Charge fuel burnt as a run of the agent ended: it is owed like any
    /// other, but says nothing of a next slice.
    pub fn settle(&self, used: u64, now: Instant) {
        let mut bucket = self.bucket();
        bucket.fill(now);
        bucket.charge(used);
    }

    /// How long until the agent may run again: none if it may now.
    pub fn wait(&self, now: Instant) -> Duration {
        let mut bucket = self.bucket();
        bucket.fill(now);
        bucket.wait()
    }

    /// Wait until the agent may run again. Always ends: the budget fills
    /// at a rate above zero, and it is looked at again at least every
    /// [`NAP_AT_MOST`].
    pub async fn refilled(&self) {
        loop {
            let wait = self.wait(Instant::now());
            if wait.is_zero() {
                return;
            }
            tokio::time::sleep(wait.min(NAP_AT_MOST)).await;
        }
    }

    /// All the fuel charged to it.
    #[cfg(test)]
    pub fn used(&self) -> u64 {
        self.bucket().used
    }
}

/// Each vault's budget, by its chain and address.
static BUDGETS: OnceLock<Mutex<ByVault>> = OnceLock::new();

/// Budgets by each vault's chain and address.
type ByVault = HashMap<(String, String), Arc<Budget>>;

/// The budget of the vault at `vault` on `chain`, filling at `rate` fuel a
/// second from now on: the same for every run of its agent while this
/// executor serves it, so a restart neither fills it nor forgives what is
/// owed.
pub fn budget_of(chain: &str, vault: &str, rate: u64) -> Arc<Budget> {
    let now = Instant::now();
    let all = BUDGETS.get_or_init(Default::default);
    let mut all = all.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let budget = all
        .entry((chain.to_string(), vault.to_string()))
        .or_insert_with(|| Arc::new(Budget::new(rate, now)))
        .clone();
    budget.set_rate(rate, now);
    budget
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn a_budget_starts_empty_and_fills_at_its_rate_up_to_a_window() {
        let start = Instant::now();
        let budget = Budget::new(1_000, start);
        assert_eq!(budget.wait(start), Duration::from_millis(1));
        assert!(!budget.spend(0, start + SECOND / 2));
        assert_eq!(budget.bucket().level, 500);
        // Idle for an hour, it holds a window's worth and no more.
        budget.wait(start + Duration::from_secs(3_600));
        assert_eq!(budget.bucket().level, 1_000);
    }

    #[test]
    fn fuel_burnt_past_the_budget_is_owed_and_waited_out() {
        let start = Instant::now();
        let budget = Budget::new(1_000, start);
        budget.wait(start + SECOND);
        // A full window, and three seconds' worth more.
        assert!(budget.spend(4_000, start + SECOND));
        let bucket = budget.bucket();
        assert_eq!((bucket.level, bucket.owed), (0, 3_000));
        drop(bucket);
        // Three seconds to repay what is owed, then the next slice (twice
        // the latest, at most the window) to fill.
        assert_eq!(budget.wait(start + SECOND), 4 * SECOND);
        assert_eq!(budget.wait(start + 4 * SECOND), SECOND);
        assert_eq!(budget.wait(start + 5 * SECOND), Duration::ZERO);
    }

    #[test]
    fn an_agent_runs_on_only_while_its_next_slice_is_paid_for() {
        let start = Instant::now();
        let budget = Budget::new(1_000, start);
        budget.wait(start + SECOND);
        // 100 burnt leaves 900, which pays for two more like it.
        assert!(!budget.spend(100, start + SECOND));
        // 500 burnt leaves 400, short of the next 1,000.
        assert!(budget.spend(500, start + SECOND));
        assert_eq!(budget.wait(start + SECOND), Duration::from_millis(600));
    }

    #[test]
    fn many_small_fills_lose_nothing_to_rounding() {
        let start = Instant::now();
        // A third of a unit every nanosecond.
        let budget = Budget::new(333_333_333, start);
        for nanos in 1..=3_000u64 {
            budget.wait(start + Duration::from_nanos(nanos));
        }
        assert_eq!(budget.bucket().level, 999);
    }

    #[test]
    fn the_budget_never_overflows_or_goes_below_nothing() {
        let start = Instant::now();
        let budget = Budget::new(u64::MAX, start);
        budget.wait(start + Duration::from_secs(1 << 40));
        assert_eq!(budget.bucket().level, u64::MAX);
        for _ in 0..3 {
            assert!(budget.spend(u64::MAX, start + Duration::from_secs(1 << 40)));
        }
        let bucket = budget.bucket();
        assert_eq!((bucket.level, bucket.owed), (0, u64::MAX));
        assert_eq!(bucket.used, u64::MAX);
        drop(bucket);
        // A rate of nothing fills all the same, at the least there is.
        let none = Budget::new(0, start);
        assert!(none.spend(5, start));
        assert!(none.wait(start) < Duration::from_secs(10));
        // A clock that steps back fills nothing, and takes nothing away.
        let budget = Budget::new(1_000, start + SECOND);
        budget.wait(start);
        assert_eq!(budget.bucket().level, 0);
    }

    #[test]
    fn an_idle_agent_saves_up_no_more_than_a_window() {
        let start = Instant::now();
        let budget = Budget::new(1_000, start);
        // Sleeping a day, then yielding in a thousand small steps, buys no
        // more than the window: a burst of 1,000 and the rest owed.
        let later = start + Duration::from_secs(86_400);
        for _ in 0..1_000 {
            budget.spend(10, later);
        }
        let bucket = budget.bucket();
        assert_eq!((bucket.level, bucket.owed), (0, 9_000));
    }

    #[test]
    fn a_vault_s_budget_carries_over_a_restart_and_a_change_of_rate() {
        let vault = "0xpaced";
        let budget = budget_of("base", vault, 1_000);
        budget.spend(50_000, Instant::now());
        let owed = budget.bucket().owed;
        assert!(owed > 40_000, "{owed}");

        // Started again, at another rate: the same budget, still owed.
        let again = budget_of("base", vault, 2_000);
        assert!(Arc::ptr_eq(&budget, &again));
        assert!(again.bucket().owed > 40_000);
        assert_eq!(again.bucket().rate, 2_000);
        // Lowered, it keeps no more than the new window.
        let budget = Budget::new(10_000, Instant::now());
        budget.wait(Instant::now() + SECOND);
        budget.set_rate(10, Instant::now() + SECOND);
        assert_eq!(budget.bucket().level, 10);
        // Another vault's budget is its own.
        let other = budget_of("base", "0xother", 1_000);
        assert!(!Arc::ptr_eq(&budget_of("base", vault, 1_000), &other));
    }

    #[tokio::test(start_paused = true)]
    async fn a_parked_agent_is_woken_once_its_budget_has_filled() {
        let budget = Arc::new(Budget::new(1_000, Instant::now()));
        assert!(budget.spend(5_000, Instant::now()));
        let started = Instant::now();
        budget.refilled().await;
        let waited = started.elapsed();
        // Five seconds owed, then twice the slice (at most the window).
        assert!(waited >= 6 * SECOND, "{waited:?}");
        assert!(waited < 6 * SECOND + NAP_AT_MOST, "{waited:?}");
        assert!(budget.wait(Instant::now()).is_zero());
    }
}
