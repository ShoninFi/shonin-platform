//! How fresh the figures on screen are (States-System, "Data freshness").
//!
//! Figures more than a minute behind the chain get a slim bar under the
//! header; two minutes without reaching the API turns that into the "can't
//! reach the indexer" card at the top of the page. Stale money is never shown
//! as live. Before anything has arrived, ten seconds of nothing replaces the
//! skeletons with the same card.

use time::PrimitiveDateTime;

/// Figures this far behind the chain get the slim bar.
pub const BEHIND_AFTER_SECONDS: f64 = 60.0;
/// This long without a snapshot is "can't reach the indexer".
pub const UNREACHABLE_AFTER_SECONDS: f64 = 120.0;
/// This long with nothing at all replaces the skeletons with the card.
pub const NOTHING_AFTER_SECONDS: f64 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Freshness {
    /// Nothing has arrived yet; skeletons show.
    Loading,
    /// Within a minute of the chain.
    Fresh,
    /// Behind the chain by this many seconds.
    Behind { seconds: f64 },
    /// The API hasn't answered for a while. `since` is when the figures on
    /// screen are from, if there are any.
    Unreachable { since: Option<PrimitiveDateTime> },
}

impl Freshness {
    /// Work out the state from the reader's clock (`now`, `started` and
    /// `last_ok` in milliseconds since the Unix epoch) and the chain's
    /// (`as_of`).
    pub fn of(
        now: f64,
        started: f64,
        last_ok: Option<f64>,
        as_of: Option<PrimitiveDateTime>,
    ) -> Self {
        let Some(last_ok) = last_ok else {
            return if (now - started) / 1000.0 >= NOTHING_AFTER_SECONDS {
                Freshness::Unreachable { since: None }
            } else {
                Freshness::Loading
            };
        };
        if (now - last_ok) / 1000.0 >= UNREACHABLE_AFTER_SECONDS {
            return Freshness::Unreachable { since: as_of };
        }
        let Some(as_of) = as_of else {
            return Freshness::Fresh;
        };
        // A chain whose clock runs ahead of the reader's (a local chain
        // moves an hour a block) is simply fresh.
        let behind = (now / 1000.0 - unix_seconds(as_of)).max(0.0);
        if behind > BEHIND_AFTER_SECONDS {
            Freshness::Behind { seconds: behind }
        } else {
            Freshness::Fresh
        }
    }

    pub fn is_stale(&self) -> bool {
        matches!(
            self,
            Freshness::Behind { .. } | Freshness::Unreachable { .. }
        )
    }
}

/// Seconds since the Unix epoch of a UTC date and time.
pub fn unix_seconds(at: PrimitiveDateTime) -> f64 {
    at.assume_utc().unix_timestamp() as f64
}

/// A UTC date and time from milliseconds since the Unix epoch.
pub fn from_unix_millis(millis: f64) -> PrimitiveDateTime {
    let at = time::OffsetDateTime::from_unix_timestamp((millis / 1000.0).floor() as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    PrimitiveDateTime::new(at.date(), at.time())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const MS: f64 = 1000.0;

    #[test]
    fn nothing_yet_is_loading_for_ten_seconds_then_unreachable() {
        assert_eq!(Freshness::of(9.0 * MS, 0.0, None, None), Freshness::Loading);
        assert_eq!(
            Freshness::of(10.0 * MS, 0.0, None, None),
            Freshness::Unreachable { since: None }
        );
    }

    #[test]
    fn figures_more_than_a_minute_behind_the_chain_are_behind() {
        let as_of = datetime!(2026-10-12 10:00);
        let now = unix_seconds(as_of) * MS;
        assert_eq!(
            Freshness::of(now + 60.0 * MS, 0.0, Some(now), Some(as_of)),
            Freshness::Fresh
        );
        assert_eq!(
            Freshness::of(now + 61.0 * MS, 0.0, Some(now + 61.0 * MS), Some(as_of)),
            Freshness::Behind { seconds: 61.0 }
        );
    }

    #[test]
    fn two_minutes_without_an_answer_is_unreachable_and_says_when_the_figures_are_from() {
        let as_of = datetime!(2026-10-12 10:00);
        let ok = unix_seconds(as_of) * MS;
        assert!(matches!(
            Freshness::of(ok + 119.0 * MS, 0.0, Some(ok), Some(as_of)),
            Freshness::Behind { .. }
        ));
        assert_eq!(
            Freshness::of(ok + 120.0 * MS, 0.0, Some(ok), Some(as_of)),
            Freshness::Unreachable { since: Some(as_of) }
        );
    }

    #[test]
    fn a_chain_clock_ahead_of_the_reader_is_fresh() {
        let as_of = datetime!(2026-10-13 10:00);
        let now = unix_seconds(datetime!(2026-10-12 10:00)) * MS;
        assert_eq!(
            Freshness::of(now, 0.0, Some(now), Some(as_of)),
            Freshness::Fresh
        );
    }

    #[test]
    fn unix_time_round_trips() {
        let at = datetime!(2026-10-12 10:17:23);
        assert_eq!(from_unix_millis(unix_seconds(at) * MS), at);
    }
}
