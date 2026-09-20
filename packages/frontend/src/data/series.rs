//! The maths behind every record in the app.
//!
//! A record is a daily series. Everything shown beside it — annualised return,
//! realised volatility, the risk-normalised series, drawdown, monthly returns —
//! is computed from that series here, so the figures and the charts can never
//! disagree with each other.
//!
//! The series themselves come from a seeded generator, so a strategy's record
//! is the same on every machine and on every build.

use shared::{Performance, Point, Strategy};
use time::{Date, Duration, Month};

/// Trading days in a year, for annualising a daily series.
const DAYS_PER_YEAR: f64 = 365.0;

/// The half-life of the volatility estimate, in days.
const VOL_HALF_LIFE: f64 = 60.0;

/// The most a strategy's returns are ever scaled up when normalising, so a
/// strategy that barely trades can't be inflated into a leader.
const MAX_SCALE_UP: f64 = 3.0;

/// A small, fast, fully deterministic generator. Seeded from a string, so a
/// strategy's series follows from its name.
pub struct Rng(u64);

impl Rng {
    pub fn seeded(seed: &str) -> Self {
        // FNV-1a, which is plenty for scattering a name into a seed.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in seed.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
        Self(hash | 1)
    }

    /// splitmix64.
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in [low, high).
    pub fn range(&mut self, low: f64, high: f64) -> f64 {
        low + self.unit() * (high - low)
    }

    /// An integer in [low, high].
    pub fn int(&mut self, low: i64, high: i64) -> i64 {
        if high <= low {
            return low;
        }
        let span = (high - low + 1) as u64;
        low + (self.next_u64() % span) as i64
    }

    /// True with the given probability.
    pub fn chance(&mut self, probability: f64) -> bool {
        self.unit() < probability
    }

    /// Standard normal, by Box-Muller.
    pub fn normal(&mut self) -> f64 {
        let u1 = self.unit().max(f64::MIN_POSITIVE);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (core::f64::consts::TAU * u2).cos()
    }

    /// One of the choices, uniformly.
    pub fn pick<'a, T>(&mut self, choices: &'a [T]) -> &'a T {
        let index = (self.next_u64() as usize) % choices.len();
        &choices[index]
    }
}

/// What a strategy's record should look like. The generator turns this into
/// a series; nothing downstream knows these numbers.
#[derive(Clone, Copy, Debug)]
pub struct Shape {
    pub days: u32,
    /// Drift, as an annualised fraction.
    pub drift: f64,
    /// Volatility, as an annualised fraction.
    pub volatility: f64,
    /// Capital at the start and at the end of the record.
    pub aum_from: f64,
    pub aum_to: f64,
    /// A bad day: how far through the record, and how far it falls.
    pub shock: Option<(f64, f64)>,
    /// Submissions a day, on average, for the trade count.
    pub trades_per_day: f64,
}

impl Shape {
    pub fn new(days: u32, drift: f64, volatility: f64, aum_from: f64, aum_to: f64) -> Self {
        Self {
            days,
            drift,
            volatility,
            aum_from,
            aum_to,
            shock: None,
            trades_per_day: 12.0,
        }
    }

    pub fn with_shock(mut self, at: f64, size: f64) -> Self {
        self.shock = Some((at, size));
        self
    }

    pub fn with_trades(mut self, per_day: f64) -> Self {
        self.trades_per_day = per_day;
        self
    }
}

/// Build a strategy's whole record from its shape, ending on `as_of`.
pub fn record(seed: &str, as_of: Date, shape: Shape) -> shared::Record {
    let mut rng = Rng::seeded(seed);
    let days = shape.days.max(2) as usize;
    let start = as_of - Duration::days(days as i64 - 1);

    let daily_drift = shape.drift / DAYS_PER_YEAR;
    let daily_vol = shape.volatility / DAYS_PER_YEAR.sqrt();
    let shock_day = shape
        .shock
        .map(|(at, _)| ((days as f64 - 1.0) * at.clamp(0.0, 1.0)) as usize);

    let mut returns = Vec::with_capacity(days);
    returns.push(0.0);
    for day in 1..days {
        let mut r = daily_drift + daily_vol * rng.normal();
        if Some(day) == shock_day {
            r += shape.shock.map_or(0.0, |(_, size)| size);
        }
        returns.push(r);
    }

    // Value per share, from 1.00 on the day the vault opened.
    let mut value_per_share = Vec::with_capacity(days);
    let mut level = 1.0;
    for (day, r) in returns.iter().enumerate() {
        level *= 1.0 + r;
        value_per_share.push(Point {
            date: start + Duration::days(day as i64),
            value: level,
        });
    }

    let normalised = normalise(&returns, &value_per_share);
    let drawdown = drawdown(&value_per_share);
    let aum = aum(&mut rng, &value_per_share, shape.aum_from, shape.aum_to);
    let monthly = monthly(&value_per_share);

    let average_aum = aum.iter().map(|p| p.value).sum::<f64>() / aum.len() as f64;
    let peak_aum = aum.iter().map(|p| p.value).fold(0.0, f64::max);

    shared::Record {
        trades: (shape.trades_per_day * days as f64) as u32,
        value_per_share,
        normalised,
        drawdown,
        aum,
        monthly,
        average_aum,
        peak_aum,
    }
}

/// Rescale a return series towards the platform's volatility target, using
/// only the days before each one, so the scaling carries no hindsight.
fn normalise(returns: &[f64], dates: &[Point]) -> Vec<Point> {
    let decay = 0.5f64.powf(1.0 / VOL_HALF_LIFE);
    let mut variance = 0.0;
    // An exponentially weighted average that starts at zero reads as far too
    // quiet for its first weeks, which would push the scaling to its cap
    // exactly where a record is least understood. Correcting for the weight
    // seen so far removes that.
    let mut weight = 0.0;
    let mut level = 1.0;
    let mut out = Vec::with_capacity(returns.len());

    for (day, r) in returns.iter().enumerate() {
        // The estimate uses the days before this one only, so the scaling
        // carries no hindsight.
        let estimate = if weight > 0.0 { variance / weight } else { 0.0 };
        let realised = (estimate * DAYS_PER_YEAR).sqrt();
        // Too little history to rescale from at all.
        let scale = if day < 20 || realised <= 0.0 {
            1.0
        } else {
            (Strategy::VOLATILITY_TARGET / realised).min(MAX_SCALE_UP)
        };
        level *= 1.0 + scale * r;
        out.push(Point {
            date: dates[day].date,
            value: level,
        });

        variance = decay * variance + (1.0 - decay) * r * r;
        weight = decay * weight + (1.0 - decay);
    }
    out
}

/// How far below its own high-water mark the series sat on each day.
fn drawdown(series: &[Point]) -> Vec<Point> {
    let mut peak = f64::MIN;
    series
        .iter()
        .map(|point| {
            peak = peak.max(point.value);
            Point {
                date: point.date,
                value: point.value / peak - 1.0,
            }
        })
        .collect()
}

/// Capital behind the strategy: a walk from where it started to where it is,
/// which is what "the size this record was earned at" is read from.
fn aum(rng: &mut Rng, series: &[Point], from: f64, to: f64) -> Vec<Point> {
    let last = (series.len() - 1).max(1) as f64;
    let mut level = from;
    series
        .iter()
        .enumerate()
        .map(|(day, point)| {
            // Deposits arrive unevenly, and follow the record upwards.
            let along = day as f64 / last;
            let target = from + (to - from) * along.powf(1.6);
            level += (target - level) * 0.08 + level * rng.range(-0.012, 0.014);
            Point {
                date: point.date,
                value: level.max(from * 0.25),
            }
        })
        .collect()
}

/// Return in each calendar month the series covers, dated to the month's end.
fn monthly(series: &[Point]) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::new();
    let mut month_start = series[0].value;
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

/// Summarise a series the way a record page does.
pub fn performance(series: &[Point], normalised: &[Point]) -> Performance {
    let days = series.len().max(2);
    let years = (days as f64 - 1.0) / DAYS_PER_YEAR;
    let total = series[days - 1].value / series[0].value - 1.0;
    let normalised_total = normalised[days - 1].value / normalised[0].value - 1.0;

    Performance {
        raw_annualised: annualise(total, years),
        normalised_annualised: annualise(normalised_total, years),
        volatility: volatility(series),
        max_drawdown: drawdown(series).iter().map(|p| p.value).fold(0.0, f64::min),
        total_return: total,
        days: (days - 1) as u32,
    }
}

fn annualise(total: f64, years: f64) -> f64 {
    if years <= 0.0 {
        return 0.0;
    }
    (1.0 + total).max(0.000_001).powf(1.0 / years) - 1.0
}

/// Realised annualised volatility of a level series.
pub fn volatility(series: &[Point]) -> f64 {
    let returns: Vec<f64> = series
        .windows(2)
        .map(|pair| pair[1].value / pair[0].value - 1.0)
        .collect();
    if returns.len() < 2 {
        return 0.0;
    }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance =
        returns.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>() / (returns.len() - 1) as f64;
    (variance * DAYS_PER_YEAR).sqrt()
}

/// How two records moved together: 1.0 is the same risk twice over, 0.0 is
/// diversification. Computed over the days the two series share.
pub fn correlation(left: &[Point], right: &[Point]) -> f64 {
    let span = left.len().min(right.len());
    if span < 30 {
        return 0.0;
    }
    let daily = |series: &[Point]| -> Vec<f64> {
        series[series.len() - span..]
            .windows(2)
            .map(|pair| pair[1].value / pair[0].value - 1.0)
            .collect()
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use time::Month;

    fn as_of() -> Date {
        Date::from_calendar_date(2026, Month::September, 20).unwrap()
    }

    #[test]
    fn a_seed_always_gives_the_same_series() {
        let shape = Shape::new(200, 0.18, 0.09, 500_000.0, 3_200_000.0);
        let first = record("basis", as_of(), shape);
        let second = record("basis", as_of(), shape);
        assert_eq!(
            first.value_per_share.last().unwrap().value,
            second.value_per_share.last().unwrap().value
        );
    }

    #[test]
    fn different_seeds_give_different_series() {
        let shape = Shape::new(200, 0.18, 0.09, 500_000.0, 3_200_000.0);
        let first = record("basis", as_of(), shape);
        let second = record("quoter", as_of(), shape);
        assert_ne!(
            first.value_per_share.last().unwrap().value,
            second.value_per_share.last().unwrap().value
        );
    }

    #[test]
    fn the_record_ends_on_the_day_it_was_read() {
        let record = record("basis", as_of(), Shape::new(90, 0.1, 0.1, 1.0, 2.0));
        assert_eq!(record.value_per_share.last().unwrap().date, as_of());
        assert_eq!(record.value_per_share.len(), 90);
    }

    #[test]
    fn normalising_pulls_volatility_towards_the_target() {
        // A deliberately quiet strategy is scaled up, and a wild one down.
        let quiet = record("quiet", as_of(), Shape::new(400, 0.04, 0.03, 1.0, 2.0));
        let wild = record("wild", as_of(), Shape::new(400, 0.04, 0.60, 1.0, 2.0));

        let closer_to_target = |raw: &[Point], normalised: &[Point]| {
            let target = Strategy::VOLATILITY_TARGET;
            (volatility(normalised) - target).abs() < (volatility(raw) - target).abs()
        };
        assert!(closer_to_target(&quiet.value_per_share, &quiet.normalised));
        assert!(closer_to_target(&wild.value_per_share, &wild.normalised));
    }

    #[test]
    fn drawdown_is_never_positive_and_starts_flat() {
        let record = record("basis", as_of(), Shape::new(300, 0.2, 0.2, 1.0, 2.0));
        assert!(record.drawdown.iter().all(|p| p.value <= 0.000_001));
        assert!(record.drawdown[0].value.abs() < 1e-9);
    }

    #[test]
    fn a_series_correlates_perfectly_with_itself() {
        let record = record("basis", as_of(), Shape::new(200, 0.2, 0.2, 1.0, 2.0));
        let self_correlation = correlation(&record.value_per_share, &record.value_per_share);
        assert!((self_correlation - 1.0).abs() < 1e-9);
    }

    #[test]
    fn monthly_returns_cover_every_month_once() {
        let record = record("basis", as_of(), Shape::new(365, 0.1, 0.1, 1.0, 2.0));
        assert!(record.monthly.len() >= 12);
        assert!(record.monthly.len() <= 14);
    }

    #[test]
    fn performance_reads_the_series_it_was_given() {
        let record = record("basis", as_of(), Shape::new(365, 0.18, 0.09, 1.0, 2.0));
        let performance = performance(&record.value_per_share, &record.normalised);
        let total = record.value_per_share.last().unwrap().value - 1.0;
        assert!((performance.total_return - total).abs() < 1e-9);
        assert_eq!(performance.days, 364);
        assert!(performance.max_drawdown <= 0.0);
    }
}
