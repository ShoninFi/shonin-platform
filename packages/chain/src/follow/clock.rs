//! The chain clock: whether a chain's head keeps pace with the wall clock
//! closely enough for wakes to be scheduled from it.
//!
//! Each observation gives one sample: the wall clock when its answer arrived
//! and the head's timestamp. `lag = wall − head time`. The chain is bounded
//! while, for each of the last `clock_settle_observations` samples,
//! `−head_ahead_max_ms ≤ lag ≤ head_lag_max_ms`, and chain time advanced at
//! the wall clock's rate (Δchain/Δwall within [1/2, 2]) over at least
//! [`RATE_SPAN_MS`] of wall time. Anything else (an endpoint behind, a
//! skewed clock, a local node's fast clock, too few samples) is unbounded,
//! and the caller takes the active path. The wall-clock estimate is used
//! only to schedule wakes; decisions use the observed head's timestamp.

use std::collections::VecDeque;

/// The shortest wall-clock span over which the rate is judged. Chains with
/// one-second timestamps and several blocks a second need a span of
/// several seconds for the rate to mean anything.
pub const RATE_SPAN_MS: u64 = 5_000;

/// How many samples are kept: enough to find one [`RATE_SPAN_MS`] back at
/// the fastest accepted pace.
const KEPT: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sample {
    wall_ms: u64,
    chain_ms: u64,
}

impl Sample {
    fn lag(self) -> i128 {
        i128::from(self.wall_ms) - i128::from(self.chain_ms)
    }
}

/// Why a chain is not bounded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unbounded {
    /// Fewer samples than the settle count.
    Settling { have: usize, need: usize },
    /// A recent head was older than the lag bound allows.
    Behind { lag_ms: i128, max_ms: u64 },
    /// A recent head was ahead of the wall clock by more than allowed.
    Ahead { ahead_ms: i128, max_ms: u64 },
    /// Chain time did not advance at the wall clock's rate.
    Rate { chain_ms: u64, wall_ms: u64 },
    /// No sample is far enough back to judge the rate yet.
    NoSpan,
    /// The wall clock or the chain's went backwards between samples.
    Backwards,
}

/// One chain's clock, judged from its observations.
#[derive(Clone, Debug)]
pub struct ChainClock {
    samples: VecDeque<Sample>,
    head_lag_max_ms: u64,
    head_ahead_max_ms: u64,
    settle: usize,
}

impl ChainClock {
    pub fn new(
        head_lag_max_ms: u64,
        head_ahead_max_ms: u64,
        clock_settle_observations: u64,
    ) -> Self {
        Self {
            samples: VecDeque::new(),
            head_lag_max_ms,
            head_ahead_max_ms,
            settle: usize::try_from(clock_settle_observations.max(1)).unwrap_or(usize::MAX),
        }
    }

    /// One observation: its head's timestamp (whole seconds, as blocks
    /// carry it) and the wall clock when its answer arrived.
    pub fn note(&mut self, head_timestamp_s: u64, wall_ms: u64) {
        let sample = Sample {
            wall_ms,
            chain_ms: head_timestamp_s.saturating_mul(1_000),
        };
        if self.samples.len() == KEPT {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    /// Forget every sample (the next ones start settling again).
    pub fn reset(&mut self) {
        self.samples.clear();
    }

    /// Whether wakes may be scheduled from this chain's clock.
    pub fn bounded(&self) -> bool {
        self.state().is_ok()
    }

    /// `Ok` while bounded, or why not.
    pub fn state(&self) -> Result<(), Unbounded> {
        if self.samples.len() < self.settle {
            return Err(Unbounded::Settling {
                have: self.samples.len(),
                need: self.settle,
            });
        }
        let recent = self.samples.range(self.samples.len() - self.settle..);
        let mut previous: Option<Sample> = None;
        for sample in recent.clone() {
            let lag = sample.lag();
            if lag > i128::from(self.head_lag_max_ms) {
                return Err(Unbounded::Behind {
                    lag_ms: lag,
                    max_ms: self.head_lag_max_ms,
                });
            }
            if -lag > i128::from(self.head_ahead_max_ms) {
                return Err(Unbounded::Ahead {
                    ahead_ms: -lag,
                    max_ms: self.head_ahead_max_ms,
                });
            }
            if let Some(previous) = previous
                && (sample.wall_ms < previous.wall_ms || sample.chain_ms < previous.chain_ms)
            {
                return Err(Unbounded::Backwards);
            }
            previous = Some(*sample);
        }
        // The rate, from the newest sample back to the newest one at least
        // RATE_SPAN_MS of wall time older.
        let newest = *self.samples.back().expect("at least one sample");
        let Some(older) = self
            .samples
            .iter()
            .rev()
            .find(|sample| newest.wall_ms.saturating_sub(sample.wall_ms) >= RATE_SPAN_MS)
        else {
            return Err(Unbounded::NoSpan);
        };
        if newest.chain_ms < older.chain_ms {
            return Err(Unbounded::Backwards);
        }
        let wall = newest.wall_ms - older.wall_ms;
        let chain = newest.chain_ms - older.chain_ms;
        // 1/2 ≤ chain/wall ≤ 2, in whole numbers.
        if chain.saturating_mul(2) < wall || chain > wall.saturating_mul(2) {
            return Err(Unbounded::Rate {
                chain_ms: chain,
                wall_ms: wall,
            });
        }
        Ok(())
    }

    /// The wall-clock time (ms) at which the head should carry
    /// `chain_time_s`, for scheduling a wake: the chain time plus the
    /// smallest lag seen over the settled samples, so the wake is early
    /// rather than late. `None` while unbounded.
    pub fn to_wall_ms(&self, chain_time_s: u64) -> Option<u64> {
        self.state().ok()?;
        let lag = self
            .samples
            .range(self.samples.len() - self.settle..)
            .map(|sample| sample.lag())
            .min()?;
        let wall = i128::from(chain_time_s.saturating_mul(1_000)) + lag;
        u64::try_from(wall.max(0)).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_700_000_000;

    /// A chain whose blocks come every `block_ms`, observed every
    /// `every_ms`, each answer arriving `lag_ms` after the head's block.
    fn run(
        clock: &mut ChainClock,
        from_ms: u64,
        count: u64,
        every_ms: u64,
        lag_ms: i64,
        rate: u64,
    ) {
        for i in 0..count {
            let wall = from_ms + i * every_ms;
            let chain_ms = (T0 * 1_000) + (wall - T0 * 1_000) * rate;
            let chain_s = chain_ms / 1_000;
            clock.note(chain_s, (wall as i64 + lag_ms) as u64);
        }
    }

    fn defaults() -> ChainClock {
        ChainClock::new(3_000, 2_000, 3)
    }

    #[test]
    fn a_head_keeping_pace_with_the_wall_clock_is_bounded_once_settled() {
        let mut clock = defaults();
        run(&mut clock, T0 * 1_000, 2, 5_000, 1_000, 1);
        assert!(matches!(
            clock.state(),
            Err(Unbounded::Settling { have: 2, need: 3 })
        ));
        run(&mut clock, T0 * 1_000 + 10_000, 1, 5_000, 1_000, 1);
        assert_eq!(clock.state(), Ok(()));
        // The wake for a chain time is that time plus the smallest lag.
        assert_eq!(clock.to_wall_ms(T0 + 20), Some((T0 + 20) * 1_000 + 1_000));
    }

    #[test]
    fn a_lag_just_inside_the_bound_is_bounded_and_just_outside_is_not() {
        for (lag, bounded) in [
            (3_000i64, true),
            (3_001, false),
            (-2_000, true),
            (-2_001, false),
        ] {
            let mut clock = defaults();
            run(&mut clock, T0 * 1_000, 4, 2_000, lag, 1);
            assert_eq!(clock.bounded(), bounded, "lag {lag}: {:?}", clock.state());
        }
    }

    #[test]
    fn a_fast_clock_is_never_bounded() {
        // A local node that moves its clock an hour every second.
        let mut clock = ChainClock::new(30_000, 10_000, 1);
        run(&mut clock, T0 * 1_000, 10, 1_000, 0, 3_600);
        assert!(!clock.bounded());
        assert_eq!(clock.to_wall_ms(T0), None);
    }

    #[test]
    fn a_rate_outside_a_half_to_double_is_unbounded_even_within_the_lag_bounds() {
        // Within generous lag bounds, chain time running at three times the
        // wall clock's rate over the span is refused; at the bounds' edges
        // (half and double) it is accepted.
        for (chain_per_wall, bounded) in [
            ((1u64, 2u64), true),
            ((2, 1), true),
            ((3, 1), false),
            ((2, 5), false),
        ] {
            let mut clock = ChainClock::new(30_000, 10_000, 1);
            let wall_from = T0 * 1_000;
            let (num, den) = chain_per_wall;
            clock.note(T0, wall_from);
            let wall_span = 10_000;
            let chain_span = wall_span * num / den;
            clock.note(T0 + chain_span / 1_000, wall_from + wall_span);
            assert_eq!(
                clock.bounded(),
                bounded,
                "{chain_per_wall:?}: {:?}",
                clock.state()
            );
        }
    }

    #[test]
    fn a_stalled_head_becomes_unbounded_and_settles_again_when_it_moves() {
        let mut clock = defaults();
        run(&mut clock, T0 * 1_000, 5, 2_000, 1_000, 1);
        assert!(clock.bounded());
        // The head stops at T0 + 8 while the wall clock goes on.
        let stalled = T0 + 8;
        for i in 0..3 {
            clock.note(stalled, (T0 + 10 + 2 * i) * 1_000);
        }
        assert!(matches!(clock.state(), Err(Unbounded::Behind { .. })));
        run(&mut clock, (T0 + 20) * 1_000, 3, 3_000, 1_000, 1);
        assert!(clock.bounded(), "{:?}", clock.state());
    }

    #[test]
    fn samples_closer_than_the_rate_span_are_not_enough() {
        let mut clock = defaults();
        run(&mut clock, T0 * 1_000, 3, 1_000, 500, 1);
        assert_eq!(clock.state(), Err(Unbounded::NoSpan));
        run(&mut clock, (T0 + 3) * 1_000, 3, 1_000, 500, 1);
        assert!(clock.bounded(), "{:?}", clock.state());
    }

    #[test]
    fn a_head_going_backwards_is_unbounded() {
        let mut clock = ChainClock::new(30_000, 10_000, 3);
        clock.note(T0 + 10, (T0 + 10) * 1_000);
        clock.note(T0 + 16, (T0 + 16) * 1_000);
        clock.note(T0 + 15, (T0 + 17) * 1_000);
        assert_eq!(clock.state(), Err(Unbounded::Backwards));
    }
}
