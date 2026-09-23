//! The maths behind a record.
//!
//! A record is a daily series of a vault's value per share, read from its own
//! contract. Everything shown beside it — annualised return, realised
//! volatility, the risk-normalised series, drawdown, monthly returns — is
//! computed from that one series here, so the figures and the charts can never
//! disagree, and so the indexer, the API and the web app all say the same
//! thing about the same vault.

use time::Month;

use crate::model::common::{Performance, Point};
use crate::model::strategy::Record;

/// Days in a year, for annualising a daily series. Calendar days, because a
/// vault holds its positions at the weekend too.
pub const DAYS_PER_YEAR: f64 = 365.0;

/// The half-life of the volatility estimate, in days.
pub const VOL_HALF_LIFE: f64 = 60.0;

/// The most a strategy's returns are ever scaled up when normalising, so a
/// strategy that barely trades can't be inflated into a leader.
pub const MAX_SCALE_UP: f64 = 3.0;

/// The volatility every record is rescaled to, so two strategies can be
/// compared at one level of risk.
pub const VOLATILITY_TARGET: f64 = 0.15;

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

/// Rescale a series towards the volatility target, using only the days before
/// each one, so the scaling carries no hindsight.
pub fn normalise(series: &[Point]) -> Vec<Point> {
    if series.is_empty() {
        return Vec::new();
    }
    let daily = returns(series);
    let decay = 0.5f64.powf(1.0 / VOL_HALF_LIFE);
    let mut variance = 0.0;
    // An exponentially weighted average that starts at zero reads as far too
    // quiet for its first weeks, which would push the scaling to its cap
    // exactly where a record is least understood. Correcting for the weight
    // seen so far removes that.
    let mut weight = 0.0;
    let mut level = series[0].value;
    let mut out = Vec::with_capacity(series.len());
    out.push(series[0]);

    for (day, r) in daily.iter().enumerate() {
        let estimate = if weight > 0.0 { variance / weight } else { 0.0 };
        let realised = (estimate * DAYS_PER_YEAR).sqrt();
        // Too little history to rescale from at all.
        let scale = if day < 20 || realised <= 0.0 {
            1.0
        } else {
            (VOLATILITY_TARGET / realised).min(MAX_SCALE_UP)
        };
        level *= 1.0 + scale * r;
        out.push(Point {
            date: series[day + 1].date,
            value: level,
        });

        variance = decay * variance + (1.0 - decay) * r * r;
        weight = decay * weight + (1.0 - decay);
    }
    out
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

/// Realised annualised volatility of a level series.
pub fn volatility(series: &[Point]) -> f64 {
    let daily = returns(series);
    if daily.len() < 2 {
        return 0.0;
    }
    let mean = daily.iter().sum::<f64>() / daily.len() as f64;
    let variance =
        daily.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>() / (daily.len() - 1) as f64;
    (variance * DAYS_PER_YEAR).sqrt()
}

/// How two records moved together: 1.0 is the same risk twice over, 0.0 is
/// diversification. Computed over the days the two series share.
pub fn correlation(left: &[Point], right: &[Point]) -> f64 {
    let span = left.len().min(right.len());
    if span < 30 {
        return 0.0;
    }
    let daily = |series: &[Point]| -> Vec<f64> { returns(&series[series.len() - span..]) };
    let (a, b) = (daily(left), daily(right));
    let mean = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;
    let (mean_a, mean_b) = (mean(&a), mean(&b));

    let mut covariance = 0.0;
    let mut variance_a = 0.0;
    let mut variance_b = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        let (dx, dy) = (x - mean_a, y - mean_b);
        covariance += dx * dy;
        variance_a += dx * dx;
        variance_b += dy * dy;
    }
    if variance_a <= 0.0 || variance_b <= 0.0 {
        return 0.0;
    }
    (covariance / (variance_a.sqrt() * variance_b.sqrt())).clamp(-1.0, 1.0)
}

fn annualise(total: f64, years: f64) -> f64 {
    if years <= 0.0 {
        return 0.0;
    }
    (1.0 + total).max(0.000_001).powf(1.0 / years) - 1.0
}

/// Summarise a series the way a record page does.
pub fn performance(series: &[Point], normalised: &[Point]) -> Performance {
    if series.len() < 2 || normalised.len() < 2 {
        return Performance {
            raw_annualised: 0.0,
            normalised_annualised: 0.0,
            volatility: 0.0,
            max_drawdown: 0.0,
            total_return: 0.0,
            days: 0,
        };
    }
    let days = series.len();
    let years = (days as f64 - 1.0) / DAYS_PER_YEAR;
    let total = series[days - 1].value / series[0].value - 1.0;
    let normalised_total = normalised[normalised.len() - 1].value / normalised[0].value - 1.0;

    Performance {
        raw_annualised: annualise(total, years),
        normalised_annualised: annualise(normalised_total, years),
        volatility: volatility(series),
        max_drawdown: drawdown(series).iter().map(|p| p.value).fold(0.0, f64::min),
        total_return: total,
        days: (days - 1) as u32,
    }
}

impl Record {
    /// Build a record from what the chain holds: the value of a share on each
    /// day, the capital behind it on each day, and how many trades it made.
    pub fn from_series(value_per_share: Vec<Point>, aum: Vec<Point>, trades: u32) -> Self {
        let normalised = normalise(&value_per_share);
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
            normalised,
            drawdown,
            aum,
            monthly,
            trades,
            average_aum,
            peak_aum,
        }
    }

    pub fn performance(&self) -> Performance {
        performance(&self.value_per_share, &self.normalised)
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
    fn normalising_pulls_volatility_towards_the_target() {
        let loud = wobbly(400, 0.03);
        let scaled = normalise(&loud);
        let before = volatility(&loud);
        let after = volatility(&scaled);
        assert!(before > VOLATILITY_TARGET, "the test series is too quiet");
        assert!(
            after < before,
            "normalising {before:.3} left it at {after:.3}"
        );
    }

    #[test]
    fn a_quiet_series_is_never_scaled_past_the_cap() {
        let quiet = wobbly(400, 0.0002);
        let scaled = normalise(&quiet);
        assert!(volatility(&scaled) <= volatility(&quiet) * MAX_SCALE_UP + 1e-9);
    }

    #[test]
    fn the_first_weeks_are_left_alone() {
        let series = wobbly(15, 0.02);
        let scaled = normalise(&series);
        for (raw, normalised) in series.iter().zip(scaled.iter()) {
            assert!((raw.value - normalised.value).abs() < 1e-12);
        }
    }

    #[test]
    fn drawdown_is_never_positive_and_starts_flat() {
        let down = drawdown(&series(&[1.0, 1.2, 0.9, 1.1]));
        assert_eq!(down[0].value, 0.0);
        assert!(down.iter().all(|point| point.value <= 0.0));
        assert!((down[2].value - (0.9 / 1.2 - 1.0)).abs() < 1e-12);
    }

    #[test]
    fn a_series_correlates_perfectly_with_itself() {
        let one = wobbly(120, 0.01);
        assert!((correlation(&one, &one) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn too_short_a_pair_correlates_at_nothing_rather_than_guessing() {
        let short = wobbly(10, 0.01);
        assert_eq!(correlation(&short, &short), 0.0);
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
        let summary = performance(&doubled, &doubled);
        assert!((summary.total_return - 1.0).abs() < 1e-12);
        assert_eq!(summary.days, 1);
    }

    #[test]
    fn an_empty_record_says_nothing_rather_than_dividing_by_zero() {
        let record = Record::from_series(Vec::new(), Vec::new(), 0);
        let summary = record.performance();
        assert_eq!(summary.days, 0);
        assert_eq!(summary.total_return, 0.0);
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
        assert_eq!(record.normalised.len(), 60);
    }
}
