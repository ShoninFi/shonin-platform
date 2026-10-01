//! The maths behind a record.
//!
//! A record is a daily series of a vault's value per share, read from its own
//! contract. Everything shown beside it (the return over the last 30 days, the
//! return since opening, the deepest drop, monthly returns) is plain
//! arithmetic on that one series, done here, so anyone can recompute it from
//! the chain and the indexer, the API and the web app all say the same thing.

use time::{Duration, Month};

use crate::model::common::{Performance, Point};
use crate::model::strategy::Record;

/// The window of the leaderboard's headline return, in days. An agent with
/// less history than this is listed as new.
pub const WINDOW_DAYS: u32 = 30;

/// Day-to-day returns of a level series.
pub fn returns(series: &[Point]) -> Vec<f64> {
    series
        .windows(2)
        .map(|pair| {
            if pair[0].value == 0.0 {
                0.0
            } else {
                pair[1].value / pair[0].value - 1.0
            }
        })
        .collect()
}

/// How far below its own high-water mark the series sat on each day.
pub fn drawdown(series: &[Point]) -> Vec<Point> {
    let mut peak = f64::MIN;
    series
        .iter()
        .map(|point| {
            peak = peak.max(point.value);
            Point {
                date: point.date,
                value: if peak > 0.0 {
                    point.value / peak - 1.0
                } else {
                    0.0
                },
            }
        })
        .collect()
}

/// Return in each calendar month the series covers, dated to the month's end.
pub fn monthly(series: &[Point]) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::new();
    let Some(first) = series.first() else {
        return out;
    };
    let mut month_start = first.value;
    let mut current: Option<(i32, Month)> = None;

    for point in series {
        let key = (point.date.year(), point.date.month());
        match current {
            None => current = Some(key),
            Some(previous) if previous != key => {
                out.push(Point {
                    date: point.date,
                    value: point.value / month_start - 1.0,
                });
                month_start = point.value;
                current = Some(key);
            }
            _ => {}
        }
    }

    if let Some(last) = series.last() {
        out.push(Point {
            date: last.date,
            value: last.value / month_start - 1.0,
        });
    }
    out
}

/// Summarise a series the way the leaderboard and a record page do.
pub fn performance(series: &[Point]) -> Performance {
    let (Some(first), Some(last)) = (series.first(), series.last()) else {
        return Performance::default();
    };
    let since = |start: f64| {
        if start > 0.0 {
            last.value / start - 1.0
        } else {
            0.0
        }
    };
    let days = (last.date - first.date).whole_days().max(0) as u32;
    let window_start = last.date - Duration::days(WINDOW_DAYS as i64);
    // The last point on or before the window's start, if the series reaches
    // back that far.
    let return_30d = series
        .iter()
        .rev()
        .find(|point| point.date <= window_start)
        .map(|point| since(point.value));
    Performance {
        return_30d,
        return_since_open: since(first.value),
        deepest_drop: drawdown(series).iter().map(|p| p.value).fold(0.0, f64::min),
        days,
    }
}

impl Record {
    /// Build a record from what the chain holds: the value of a share on each
    /// day, the capital behind it on each day, and how many trades it made.
    pub fn from_series(value_per_share: Vec<Point>, aum: Vec<Point>, trades: u32) -> Self {
        let drawdown = drawdown(&value_per_share);
        let monthly = monthly(&value_per_share);
        let average_aum = if aum.is_empty() {
            0.0
        } else {
            aum.iter().map(|point| point.value).sum::<f64>() / aum.len() as f64
        };
        let peak_aum = aum.iter().map(|point| point.value).fold(0.0, f64::max);
        Self {
            value_per_share,
            drawdown,
            aum,
            monthly,
            trades,
            average_aum,
            peak_aum,
        }
    }

    pub fn performance(&self) -> Performance {
        performance(&self.value_per_share)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Date, Duration};

    fn series(values: &[f64]) -> Vec<Point> {
        let start = Date::from_calendar_date(2026, Month::January, 1).expect("a real date");
        values
            .iter()
            .enumerate()
            .map(|(day, value)| Point {
                date: start + Duration::days(day as i64),
                value: *value,
            })
            .collect()
    }

    /// A series that wobbles by a fixed fraction each day, which gives a
    /// volatility we can reason about.
    fn wobbly(days: usize, step: f64) -> Vec<Point> {
        let mut level = 1.0;
        let values: Vec<f64> = (0..days)
            .map(|day| {
                level *= 1.0 + if day % 2 == 0 { step } else { -step };
                level
            })
            .collect();
        series(&values)
    }

    #[test]
    fn drawdown_is_never_positive_and_starts_flat() {
        let down = drawdown(&series(&[1.0, 1.2, 0.9, 1.1]));
        assert_eq!(down[0].value, 0.0);
        assert!(down.iter().all(|point| point.value <= 0.0));
        assert!((down[2].value - (0.9 / 1.2 - 1.0)).abs() < 1e-12);
    }

    #[test]
    fn monthly_returns_cover_every_month_once() {
        let year = wobbly(400, 0.004);
        let months = monthly(&year);
        assert_eq!(months.len(), 14);
        assert!(months.iter().all(|point| point.value.abs() < 1.0));
    }

    #[test]
    fn performance_reads_the_series_it_was_given() {
        let doubled = series(&[1.0, 2.0]);
        let summary = performance(&doubled);
        assert!((summary.return_since_open - 1.0).abs() < 1e-12);
        assert_eq!(summary.days, 1);
        // A day of history has no 30-day return to show.
        assert_eq!(summary.return_30d, None);
    }

    #[test]
    fn the_thirty_day_return_starts_thirty_days_back() {
        let values: Vec<f64> = (0..41).map(|day| 1.0 + day as f64 / 100.0).collect();
        let summary = performance(&series(&values));
        // Day 40 against day 10.
        let expected = 1.40 / 1.10 - 1.0;
        assert!((summary.return_30d.unwrap() - expected).abs() < 1e-12);
        assert!((summary.return_since_open - 0.40).abs() < 1e-12);
        assert_eq!(summary.deepest_drop, 0.0);
    }

    #[test]
    fn an_empty_record_says_nothing_rather_than_dividing_by_zero() {
        let record = Record::from_series(Vec::new(), Vec::new(), 0);
        let summary = record.performance();
        assert_eq!(summary.days, 0);
        assert_eq!(summary.return_since_open, 0.0);
        assert_eq!(record.peak_aum, 0.0);
    }

    #[test]
    fn a_record_carries_the_capital_it_was_earned_at() {
        let value = wobbly(60, 0.005);
        let aum = series(
            &(0..60)
                .map(|day| 1_000.0 + day as f64 * 100.0)
                .collect::<Vec<_>>(),
        );
        let record = Record::from_series(value, aum, 42);
        assert_eq!(record.trades, 42);
        assert_eq!(record.peak_aum, 6_900.0);
        assert!((record.average_aum - 3_950.0).abs() < 1e-9);
        assert_eq!(record.drawdown.len(), 60);
    }
}
