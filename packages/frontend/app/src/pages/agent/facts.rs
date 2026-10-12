//! What the agent page says, worked out from the record: the chart's scales
//! and windows, the banners and their order, the period each figure names,
//! the words for each event, and where a transaction can be checked. Plain
//! functions with no web dependencies, so the pre-render, the app and the
//! tests agree.

use shared::{
    Area, EventKind, ExitClass, Finding, Point, Rating, Review, Severity, Source, Standing,
    Strategy, VaultState,
};
use time::{Date, Duration, PrimitiveDateTime, Weekday};

use crate::format;
use crate::model::{TankState, days_until};

// ---- The chart ------------------------------------------------------------

/// How much of the record the chart shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    Week,
    Month,
    Quarter,
    All,
}

impl Range {
    pub const ALL: [(Range, &'static str); 4] = [
        (Range::Week, "1W"),
        (Range::Month, "1M"),
        (Range::Quarter, "3M"),
        (Range::All, "All"),
    ];

    /// Days back from the latest point, or none for the whole record.
    pub fn days(self) -> Option<i64> {
        match self {
            Range::Week => Some(7),
            Range::Month => Some(30),
            Range::Quarter => Some(90),
            Range::All => None,
        }
    }
}

/// What the chart draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Price,
    Money,
}

/// The points of a series that fall in a range: everything on or after the
/// latest day less the range's days.
pub fn window(points: &[Point], range: Range) -> &[Point] {
    let (Some(last), Some(days)) = (points.last(), range.days()) else {
        return points;
    };
    let start = last.date - Duration::days(days);
    let first = points
        .iter()
        .position(|point| point.date >= start)
        .unwrap_or(0);
    &points[first..]
}

/// The y axis: four evenly spaced ticks at a round step that cover the
/// series, and the domain drawn, which leaves 5% headroom above the top tick
/// and below the bottom one.
#[derive(Clone, Debug, PartialEq)]
pub struct Scale {
    pub ticks: [f64; 4],
    pub step: f64,
    pub low: f64,
    pub high: f64,
}

/// The smallest step a chart's axis takes: a hundredth of a cent on a share
/// price, a dollar on money.
pub fn min_step(mode: Mode) -> f64 {
    match mode {
        Mode::Price => 0.0001,
        Mode::Money => 1.0,
    }
}

/// The next round step (1, 2, 2.5 or 5 times a power of ten) at or above
/// `raw`.
fn round_step(raw: f64) -> f64 {
    let power = 10f64.powf(raw.log10().floor());
    for factor in [1.0, 2.0, 2.5, 5.0, 10.0] {
        let step = factor * power;
        if step >= raw * (1.0 - 1e-9) {
            return step;
        }
    }
    10.0 * power
}

impl Scale {
    pub fn of(values: &[f64], smallest: f64) -> Scale {
        let low = values.iter().copied().fold(f64::INFINITY, f64::min);
        let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let (low, high) = if low.is_finite() && high.is_finite() {
            (low, high)
        } else {
            (0.0, 0.0)
        };
        let mut step = round_step(((high - low) / 3.0).max(smallest));
        loop {
            let first = (low / step + 1e-9).floor() * step;
            if first + 3.0 * step >= high - step * 1e-9 {
                let ticks = [first, first + step, first + 2.0 * step, first + 3.0 * step];
                let headroom = 0.05 * 3.0 * step;
                return Scale {
                    ticks,
                    step,
                    low: first - headroom,
                    high: ticks[3] + headroom,
                };
            }
            step = round_step(step * 1.0001);
        }
    }

    /// Where a value sits, from 0 at the top of the plot to 1 at the bottom.
    pub fn y(&self, value: f64) -> f64 {
        let span = self.high - self.low;
        if span <= 0.0 {
            return 0.5;
        }
        ((self.high - value) / span).clamp(0.0, 1.0)
    }
}

/// Where a day sits across the plot, from 0 at the first day to 1 at the
/// last.
pub fn x(date: Date, first: Date, last: Date) -> f64 {
    let span = (last - first).whole_days();
    if span <= 0 {
        return 0.0;
    }
    ((date - first).whole_days() as f64 / span as f64).clamp(0.0, 1.0)
}

/// The day nearest a point across the plot (0 to 1), as an index into the
/// window's points.
pub fn nearest(points: &[Point], fraction: f64) -> Option<usize> {
    let (first, last) = (points.first()?.date, points.last()?.date);
    let fraction = fraction.clamp(0.0, 1.0);
    points
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let da = (x(a.date, first, last) - fraction).abs();
            let db = (x(b.date, first, last) - fraction).abs();
            da.total_cmp(&db)
        })
        .map(|(index, _)| index)
}

/// How a label lines up with the place it names: the first to the left, the
/// last to the right, the rest centred.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Start,
    Middle,
    End,
}

/// The labels under the plot: the first and last days, and between them
/// the month starts (week starts on a window of six weeks or less), leaving
/// out any too close to the ends to read and keeping at most three, evenly
/// spread, so they fit a phone.
pub fn x_labels(first: Date, last: Date) -> Vec<(f64, Date, Anchor)> {
    let mut labels = vec![(0.0, first, Anchor::Start)];
    if last <= first {
        return labels;
    }
    let span = (last - first).whole_days();
    let mut between = Vec::new();
    let mut day = first + Duration::days(1);
    while day < last {
        let mark = if span > 42 {
            day.day() == 1
        } else if span > 10 {
            day.weekday() == Weekday::Monday
        } else {
            false
        };
        let at = x(day, first, last);
        if mark && (0.2..=0.8).contains(&at) {
            between.push((at, day, Anchor::Middle));
        }
        day += Duration::days(1);
    }
    let keep = 3;
    if between.len() > keep {
        let step = between.len() as f64 / keep as f64;
        between = (0..keep)
            .map(|index| between[((index as f64 + 0.5) * step) as usize])
            .collect();
    }
    labels.extend(between);
    labels.push((1.0, last, Anchor::End));
    labels
}

/// A value on the y axis: a price to as many decimals as the step needs (two
/// at least, four at most), money in whole dollars, in thousands or millions
/// once the step is that coarse.
pub fn axis_label(value: f64, step: f64, mode: Mode) -> String {
    match mode {
        Mode::Price => {
            let decimals = (-step.log10().floor()).clamp(2.0, 4.0) as usize;
            format!("${}", format::number(value, decimals))
        }
        Mode::Money => {
            if step >= 100_000.0 {
                let decimals = if (step / 1_000_000.0).fract() == 0.0 {
                    0
                } else {
                    1
                };
                format!("${}M", format::number(value / 1_000_000.0, decimals))
            } else if step >= 1_000.0 {
                format!("${}k", format::number(value / 1_000.0, 0))
            } else {
                format::money_whole(value)
            }
        }
    }
}

/// The figure a chart point shows: a share price to four decimals, money to
/// the dollar.
pub fn figure(value: f64, mode: Mode) -> String {
    match mode {
        Mode::Price => format::price(value),
        Mode::Money => format::money_whole(value),
    }
}

/// The deepest fall from an earlier high inside a series, as a negative
/// fraction (zero if it never fell).
pub fn deepest_drop(points: &[Point]) -> f64 {
    let mut peak = f64::NEG_INFINITY;
    let mut deepest: f64 = 0.0;
    for point in points {
        peak = peak.max(point.value);
        if peak > 0.0 {
            deepest = deepest.min(point.value / peak - 1.0);
        }
    }
    deepest
}

/// What the chart says to a screen reader, for the range it shows.
pub fn summary(points: &[Point], mode: Mode) -> String {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return "No record yet.".to_string();
    };
    let what = match mode {
        Mode::Price => "Share price",
        Mode::Money => "Money in vault",
    };
    let verb = if last.value > first.value {
        "rose"
    } else if last.value < first.value {
        "fell"
    } else {
        "held"
    };
    let days = format::days((last.date - first.date).whole_days().max(0) as u32);
    let mut words = format!(
        "{what} {verb} from {} to {} over {days}",
        figure(first.value, mode),
        figure(last.value, mode)
    );
    if mode == Mode::Price {
        words.push_str(&format!(
            "; deepest drop {}",
            format::ret(deepest_drop(points))
        ));
    }
    words.push('.');
    words
}

/// The path of a series in a 1000 x 1000 box (stretched to the plot), one
/// point per day.
pub fn path(points: &[Point], scale: &Scale) -> Option<String> {
    let (first, last) = (points.first()?.date, points.last()?.date);
    if points.len() < 2 {
        return None;
    }
    let mut out = String::new();
    for (index, point) in points.iter().enumerate() {
        out.push_str(&format!(
            "{}{:.1} {:.1}",
            if index == 0 { 'M' } else { 'L' },
            x(point.date, first, last) * 1000.0,
            scale.y(point.value) * 1000.0
        ));
    }
    Some(out)
}

// ---- Dates and periods ----------------------------------------------------

/// A date with its year: Sep 7, 2026.
pub fn full_day(date: Date) -> String {
    format::day(date, i32::MIN)
}

/// A day as an axis or a table shows it, the year left out when it is
/// `this_year`.
pub fn short_day(date: Date, this_year: i32) -> String {
    format::day(date, this_year)
}

/// Whether a moment is the record's "unknown" time (the Unix epoch).
pub fn is_unknown(at: PrimitiveDateTime) -> bool {
    at.year() <= 1970
}

/// The period a 30-day return covers: "Return, Aug 30 to Sep 29".
pub fn period_30d(strategy: &Strategy, this_year: i32) -> Option<String> {
    let last = strategy.record.as_ref()?.value_per_share.last()?.date;
    Some(format!(
        "Return, {} to {}",
        format::day(last - Duration::days(30), this_year),
        format::day(last, this_year)
    ))
}

// ---- Banners --------------------------------------------------------------

/// The banners an agent page can carry, most serious first. At most two
/// show; none can be dismissed.
#[derive(Clone, Debug, PartialEq)]
pub enum Banner {
    Killed {
        at: Option<PrimitiveDateTime>,
        reason: String,
        outcome: Option<String>,
        reference: Option<String>,
    },
    Frozen {
        at: Option<PrimitiveDateTime>,
    },
    Closed,
    Cancelled,
    Activating,
    TankEmpty,
    Serious {
        areas: Vec<Area>,
        refused: bool,
    },
    Retiring {
        since: Option<PrimitiveDateTime>,
        ends: Option<PrimitiveDateTime>,
    },
    TankLow {
        days: u32,
    },
    New {
        days: u32,
    },
    Sealed,
    Served,
}

impl Banner {
    /// Its place in the order: lower is more serious.
    pub fn rank(&self) -> u8 {
        match self {
            Banner::Killed { .. } => 0,
            Banner::Frozen { .. } => 1,
            Banner::Closed => 2,
            Banner::Cancelled => 3,
            Banner::Activating => 4,
            Banner::TankEmpty => 5,
            Banner::Serious { .. } => 6,
            Banner::Retiring { .. } => 7,
            Banner::TankLow { .. } => 8,
            Banner::New { .. } => 9,
            Banner::Sealed => 10,
            Banner::Served => 11,
        }
    }
}

/// The most banners a page shows.
pub const MAX_BANNERS: usize = 2;

/// Every banner that applies to an agent, most serious first.
pub fn all_banners(strategy: &Strategy) -> Vec<Banner> {
    let vault = &strategy.vault;
    let mut out = Vec::new();
    let state = vault.state;
    let running = matches!(state, VaultState::Open | VaultState::WindingDown);
    match state {
        VaultState::Killed => out.push(Banner::Killed {
            at: strategy
                .kill
                .as_ref()
                .map(|kill| kill.at)
                .or(vault.halt.as_ref().map(|halt| halt.at)),
            reason: strategy
                .kill
                .as_ref()
                .map(|kill| kill.reason.clone())
                .or(vault.halt.as_ref().map(|halt| halt.reason.clone()))
                .unwrap_or_default(),
            outcome: strategy.kill.as_ref().map(|kill| kill.outcome.clone()),
            reference: strategy.kill.as_ref().map(|kill| kill.reference.clone()),
        }),
        VaultState::Frozen => out.push(Banner::Frozen {
            at: vault.halt.as_ref().map(|halt| halt.at),
        }),
        VaultState::Closed => {
            if let Some(kill) = &strategy.kill {
                out.push(Banner::Killed {
                    at: Some(kill.at),
                    reason: kill.reason.clone(),
                    outcome: Some(kill.outcome.clone()),
                    reference: Some(kill.reference.clone()),
                });
            }
            out.push(Banner::Closed);
        }
        VaultState::Cancelled => out.push(Banner::Cancelled),
        VaultState::Activating => out.push(Banner::Activating),
        VaultState::WindingDown => out.push(Banner::Retiring {
            since: vault.halt.as_ref().map(|halt| halt.at),
            ends: crate::model::wind_down_ends(strategy),
        }),
        VaultState::Open => {}
    }
    let tank = TankState::of(strategy);
    if running && tank.is_empty() {
        out.push(Banner::TankEmpty);
    }
    if let Some(review) = &strategy.review
        && state != VaultState::Cancelled
        && (review.refused || review.ratings.contains(&Rating::SeriousConcerns))
    {
        out.push(Banner::Serious {
            areas: Area::ALL
                .into_iter()
                .filter(|area| review.rating(*area) == Rating::SeriousConcerns)
                .collect(),
            refused: review.refused,
        });
    }
    if running && tank.is_low() {
        out.push(Banner::TankLow {
            days: tank.days.floor().max(0.0) as u32,
        });
    }
    if state == VaultState::Open && strategy.standing == Standing::New {
        out.push(Banner::New {
            days: strategy.days_live,
        });
    }
    let shown = !matches!(state, VaultState::Cancelled | VaultState::Closed);
    if shown && matches!(strategy.bundle.source, Source::Sealed) {
        out.push(Banner::Sealed);
    }
    if shown && matches!(vault.exit_class, ExitClass::Served { .. }) {
        out.push(Banner::Served);
    }
    out.sort_by_key(Banner::rank);
    out
}

/// The banners a page shows: the most serious two.
pub fn banners(strategy: &Strategy) -> Vec<Banner> {
    let mut all = all_banners(strategy);
    all.truncate(MAX_BANNERS);
    all
}

/// The state chip under the name: its words, and whether it needs attention
/// (caution) or says something has stopped (loss).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChipTone {
    Gain,
    Neutral,
    Caution,
    Loss,
}

pub fn state_chip(strategy: &Strategy, now: Option<PrimitiveDateTime>) -> (String, ChipTone) {
    let vault = &strategy.vault;
    match vault.state {
        VaultState::Activating => ("Activating".into(), ChipTone::Neutral),
        VaultState::WindingDown => {
            let left = match (crate::model::wind_down_ends(strategy), now) {
                (Some(end), Some(now)) => {
                    let days = days_until(end, now);
                    format!(" · {} left", format::days(u32::try_from(days).unwrap_or(0)))
                }
                _ => String::new(),
            };
            (format!("Retiring{left}"), ChipTone::Caution)
        }
        VaultState::Frozen => ("Frozen".into(), ChipTone::Loss),
        VaultState::Killed => ("Killed".into(), ChipTone::Loss),
        VaultState::Closed => ("Closed".into(), ChipTone::Neutral),
        VaultState::Cancelled => ("Cancelled".into(), ChipTone::Neutral),
        VaultState::Open => {
            if strategy.standing == Standing::New {
                (
                    format!("New · {}", format::days(strategy.days_live)),
                    ChipTone::Neutral,
                )
            } else {
                ("Live".into(), ChipTone::Gain)
            }
        }
    }
}

// ---- The review -----------------------------------------------------------

/// The finding that leads an area: the most severe, the first of those.
pub fn lead_finding(review: &Review, area: Area) -> Option<&Finding> {
    review.findings_in(area).max_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then(std::cmp::Ordering::Greater)
    })
}

/// The areas whose rating needs saying out loud on the overview, most severe
/// first: serious concerns, then some concerns.
pub fn concerns(review: &Review) -> Vec<(Area, Rating)> {
    let mut out: Vec<(Area, Rating)> = Area::ALL
        .into_iter()
        .map(|area| (area, review.rating(area)))
        .filter(|(_, rating)| matches!(rating, Rating::SeriousConcerns | Rating::SomeConcerns))
        .collect();
    out.sort_by_key(|(_, rating)| match rating {
        Rating::SeriousConcerns => 0,
        _ => 1,
    });
    out
}

/// How many areas have each rating, in the order the summary reads.
pub fn rating_counts(review: &Review) -> [(Rating, Vec<Area>); 4] {
    let of = |rating: Rating| {
        Area::ALL
            .into_iter()
            .filter(|area| review.rating(*area) == rating)
            .collect::<Vec<_>>()
    };
    [
        (Rating::LooksFine, of(Rating::LooksFine)),
        (Rating::SomeConcerns, of(Rating::SomeConcerns)),
        (Rating::SeriousConcerns, of(Rating::SeriousConcerns)),
        (Rating::CouldntTell, of(Rating::CouldntTell)),
    ]
}

pub fn severity_word(severity: Severity) -> &'static str {
    match severity {
        Severity::Note => "Note",
        Severity::Concern => "Concern",
        Severity::Serious => "Serious",
    }
}

/// How much source the reviewer read, when it is public: files and lines.
pub fn source_size(strategy: &Strategy) -> Option<(usize, usize)> {
    let files = &strategy.bundle.source_files;
    if files.is_empty() || !strategy.bundle.source.is_published() {
        return None;
    }
    let lines = files.iter().map(|file| file.body.lines().count()).sum();
    Some((files.len(), lines))
}

// ---- Activity -------------------------------------------------------------

/// What an event is called in the activity list.
pub fn event_word(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Deposited => "Deposit",
        EventKind::Withdrawn => "Withdrawal",
        EventKind::ClaimBooked => "Withdrawal booked",
        EventKind::ClaimSettled => "Claim paid",
        EventKind::Crystallised => "Fee payment",
        EventKind::CapitalMoved => "Money moved",
        EventKind::TankCharged => "Tank collected",
        EventKind::TankFunded => "Tank topped up",
        EventKind::Frozen => "Frozen",
        EventKind::Killed => "Killed",
        EventKind::Blessed => "Keys blessed",
        EventKind::Valued => "Valuation",
        EventKind::Retired => "Retired",
        EventKind::Refunded => "Deposit returned",
        EventKind::Requested => "Request",
        EventKind::ClaimTried => "Claim tried",
        EventKind::FallbackTaken => "Fallback taken",
        EventKind::Closed => "Closed",
    }
}

/// An event's amount as the list shows it: money in signed for a deposit,
/// out for a withdrawal or a paid claim, plain otherwise.
pub fn event_amount(kind: EventKind, amount: Option<f64>) -> Option<String> {
    let amount = amount?;
    Some(match kind {
        EventKind::Deposited => format!("+{}", format::money(amount)),
        EventKind::Withdrawn | EventKind::ClaimSettled => {
            format!("{}{}", format::MINUS, format::money(amount))
        }
        _ => format::money(amount),
    })
}

/// A sentence that starts with a capital.
pub fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

// ---- Explorers ------------------------------------------------------------

/// Where a transaction can be checked on its chain's explorer: an EVM
/// transaction hash on an EVM network, a signature on a Solana cluster.
/// `network` is the API's key for the chain (`evm:84532`, `solana:devnet`).
pub fn tx_url(network: &str, reference: &str) -> Option<String> {
    let (family, id) = network.split_once(':')?;
    match family {
        "evm" => {
            let chain_id: u64 = id.parse().ok()?;
            let hash: alloy_primitives::B256 = reference.parse().ok()?;
            crate::wallet::networks::tx_url(chain_id, hash)
        }
        "solana" => solana_url(id, "tx", reference),
        _ => None,
    }
}

/// An address's page on its chain's explorer.
pub fn address_url(network: &str, address: &str) -> Option<String> {
    let (family, id) = network.split_once(':')?;
    match family {
        "evm" => {
            let chain_id: u64 = id.parse().ok()?;
            let address: alloy_primitives::Address = address.parse().ok()?;
            crate::wallet::networks::address_url(chain_id, address)
        }
        "solana" => solana_url(id, "address", address),
        _ => None,
    }
}

fn solana_url(cluster: &str, what: &str, value: &str) -> Option<String> {
    let base58 = !value.is_empty()
        && value.len() <= 100
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() && !matches!(c, '0' | 'O' | 'I' | 'l'));
    if !base58 {
        return None;
    }
    match cluster {
        "mainnet-beta" => Some(format!("https://explorer.solana.com/{what}/{value}")),
        "devnet" | "testnet" => Some(format!(
            "https://explorer.solana.com/{what}/{value}?cluster={cluster}"
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn series(start: Date, values: &[f64]) -> Vec<Point> {
        values
            .iter()
            .enumerate()
            .map(|(day, value)| Point {
                date: start + Duration::days(day as i64),
                value: *value,
            })
            .collect()
    }

    #[test]
    fn a_window_keeps_the_days_from_its_start_to_the_latest_day() {
        let points = series(date!(2026 - 07 - 01), &[1.0; 91]);
        assert_eq!(window(&points, Range::Week).len(), 8);
        assert_eq!(window(&points, Range::Month).len(), 31);
        assert_eq!(window(&points, Range::Quarter).len(), 91);
        assert_eq!(window(&points, Range::All).len(), 91);
        let short = series(date!(2026 - 07 - 01), &[1.0; 4]);
        assert_eq!(window(&short, Range::Month).len(), 4);
        assert!(window(&[], Range::Week).is_empty());
    }

    #[test]
    fn the_axis_takes_four_round_ticks_that_cover_the_series_with_headroom() {
        let scale = Scale::of(&[1.0012, 1.1402, 1.06], min_step(Mode::Price));
        assert_eq!(scale.step, 0.05);
        assert!((scale.ticks[0] - 1.0).abs() < 1e-9);
        assert!((scale.ticks[3] - 1.15).abs() < 1e-9);
        assert!(scale.low < scale.ticks[0] && scale.high > scale.ticks[3]);
        assert!((scale.high - scale.ticks[3] - 0.0075).abs() < 1e-9);
        for value in [1.0012, 1.1402, 1.06] {
            let y = scale.y(value);
            assert!((0.0..=1.0).contains(&y));
        }
        assert!(
            scale.y(1.1402) < scale.y(1.0012),
            "higher values sit higher"
        );
    }

    #[test]
    fn a_flat_or_tiny_series_still_gets_distinct_ticks() {
        let flat = Scale::of(&[1.000957, 1.000957], min_step(Mode::Price));
        assert_eq!(flat.step, 0.0001);
        assert!(flat.ticks.windows(2).all(|pair| pair[1] > pair[0]));
        assert!(flat.ticks[0] <= 1.000957 && flat.ticks[3] >= 1.000957);
        let labels: Vec<String> = flat
            .ticks
            .iter()
            .map(|tick| axis_label(*tick, flat.step, Mode::Price))
            .collect();
        let mut unique = labels.clone();
        unique.dedup();
        assert_eq!(unique.len(), 4, "{labels:?}");
        let money = Scale::of(&[25_000.0, 25_006.0], min_step(Mode::Money));
        assert!(money.step >= 1.0);
        assert!(money.ticks[0] <= 25_000.0 && money.ticks[3] >= 25_006.0);
    }

    #[test]
    fn money_labels_shorten_only_when_the_step_is_coarse() {
        assert_eq!(axis_label(2_000_000.0, 1_000_000.0, Mode::Money), "$2M");
        assert_eq!(axis_label(1_500_000.0, 500_000.0, Mode::Money), "$1.5M");
        assert_eq!(axis_label(25_000.0, 5_000.0, Mode::Money), "$25k");
        assert_eq!(axis_label(25_005.0, 5.0, Mode::Money), "$25,005");
        assert_eq!(axis_label(1.15, 0.05, Mode::Price), "$1.15");
        assert_eq!(axis_label(1.0008, 0.0001, Mode::Price), "$1.0008");
    }

    #[test]
    fn days_spread_across_the_plot_by_date_and_snap_to_the_nearest() {
        let points = series(date!(2026 - 09 - 01), &[1.0, 1.1, 1.2, 1.3, 1.4]);
        let (first, last) = (points[0].date, points[4].date);
        assert_eq!(x(points[2].date, first, last), 0.5);
        assert_eq!(nearest(&points, 0.0), Some(0));
        assert_eq!(nearest(&points, 0.6), Some(2));
        assert_eq!(nearest(&points, 0.63), Some(3));
        assert_eq!(nearest(&points, 2.0), Some(4));
        assert_eq!(nearest(&[], 0.5), None);
    }

    #[test]
    fn x_labels_name_the_ends_and_the_month_starts_between() {
        let labels = x_labels(date!(2026 - 07 - 01), date!(2026 - 09 - 29));
        let days: Vec<Date> = labels.iter().map(|(_, day, _)| *day).collect();
        assert_eq!(
            days,
            [
                date!(2026 - 07 - 01),
                date!(2026 - 08 - 01),
                date!(2026 - 09 - 01),
                date!(2026 - 09 - 29)
            ]
        );
        assert_eq!(labels[0].2, Anchor::Start);
        assert_eq!(labels[3].2, Anchor::End);
        assert_eq!(labels[1].2, Anchor::Middle);
        let week = x_labels(date!(2026 - 10 - 05), date!(2026 - 10 - 12));
        assert_eq!(week.len(), 2);
        let year = x_labels(date!(2025 - 10 - 01), date!(2026 - 09 - 30));
        assert_eq!(year.len(), 5, "at most three between the ends");
        assert!(year[1..4].iter().all(|(at, _, _)| (0.2..=0.8).contains(at)));
    }

    #[test]
    fn the_summary_names_the_range_the_ends_and_the_deepest_drop() {
        let points = series(date!(2026 - 07 - 01), &[1.0, 1.1, 0.99, 1.1402]);
        assert_eq!(
            summary(&points, Mode::Price),
            "Share price rose from $1.0000 to $1.1402 over 3 days; deepest drop \u{2212}10.0%."
        );
        assert_eq!(
            deepest_drop(&series(date!(2026 - 07 - 01), &[1.0, 1.2])),
            0.0
        );
    }

    #[test]
    fn a_path_needs_two_days_and_spans_the_box() {
        let points = series(date!(2026 - 07 - 01), &[1.0, 1.15]);
        let scale = Scale::of(&[1.0, 1.15], min_step(Mode::Price));
        let drawn = path(&points, &scale).unwrap();
        assert!(drawn.starts_with("M0.0 "));
        assert!(drawn.contains("L1000.0 "));
        assert_eq!(path(&points[..1], &scale), None);
    }

    #[test]
    fn explorer_links_go_to_the_vaults_own_network_and_refuse_anything_else() {
        let hash = "0x64f909791f456883a66fe4dd402e58679f60a56650de45252d6aa1249a97fb71";
        assert_eq!(
            tx_url("evm:84532", hash).as_deref(),
            Some(format!("https://sepolia.basescan.org/tx/{hash}").as_str())
        );
        assert_eq!(
            tx_url("evm:31337", hash),
            None,
            "a local chain has no explorer"
        );
        assert_eq!(tx_url("evm:84532", "javascript:alert(1)"), None);
        let signature = "2tYfRfN6ZfqVJ7DXrZ1gpecy2Wne1LtezqXefSZR1rghRyBS4h1dNKrpw69kG5tqCPhHFHC1FESFfcvkU3x5Gbw6";
        assert_eq!(
            tx_url("solana:devnet", signature).as_deref(),
            Some(format!("https://explorer.solana.com/tx/{signature}?cluster=devnet").as_str())
        );
        assert_eq!(tx_url("solana:localnet", signature), None);
        assert_eq!(tx_url("solana:devnet", "a/b?c"), None);
    }

    /// Any agent from the recorded snapshot with a review, to shape into
    /// each state.
    fn fixture() -> Strategy {
        let snapshot: shared::Snapshot =
            serde_json::from_str(include_str!("../../../tests/snapshot.json"))
                .expect("the fixture decodes");
        snapshot
            .strategies
            .into_iter()
            .find(|strategy| strategy.review.is_some())
            .expect("the fixture has a reviewed agent")
    }

    /// A live, listed agent with nothing to say: no banner at all.
    fn quiet() -> Strategy {
        let mut strategy = fixture();
        strategy.vault.state = VaultState::Open;
        strategy.standing = Standing::Listed;
        strategy.vault.halt = None;
        strategy.kill = None;
        strategy.vault.tank.balance = 600.0;
        strategy.vault.tank.daily_charge = 10.0;
        strategy.vault.exit_class = ExitClass::Direct;
        strategy.bundle.source = Source::Published {
            repo: "r".into(),
            commit: "c".into(),
        };
        if let Some(review) = strategy.review.as_mut() {
            review.ratings = [Rating::LooksFine; 6];
            review.refused = false;
        }
        strategy
    }

    fn ranks(banners: &[Banner]) -> Vec<u8> {
        banners.iter().map(Banner::rank).collect()
    }

    #[test]
    fn a_quiet_live_agent_carries_no_banner() {
        assert!(all_banners(&quiet()).is_empty());
    }

    #[test]
    fn at_most_two_banners_show_and_the_most_serious_come_first() {
        let mut strategy = quiet();
        strategy.standing = Standing::New;
        strategy.bundle.source = Source::Sealed;
        strategy.vault.exit_class = ExitClass::Served { operators: 1 };
        strategy.vault.tank.balance = 30.0; // three days at 10 a day
        if let Some(review) = strategy.review.as_mut() {
            review.ratings[1] = Rating::SeriousConcerns;
        }
        let all = all_banners(&strategy);
        assert!(matches!(all[0], Banner::Serious { .. }));
        assert!(matches!(all[1], Banner::TankLow { days: 3 }));
        assert!(matches!(all[2], Banner::New { .. }));
        assert_eq!(all[3], Banner::Sealed);
        assert_eq!(all[4], Banner::Served);
        let shown = banners(&strategy);
        assert_eq!(shown.len(), MAX_BANNERS);
        assert_eq!(shown, all[..2]);
    }

    #[test]
    fn the_order_is_kill_freeze_tank_empty_serious_retiring_tank_low_new_sealed_served() {
        let order = [
            Banner::Killed {
                at: None,
                reason: String::new(),
                outcome: None,
                reference: None,
            },
            Banner::Frozen { at: None },
            Banner::TankEmpty,
            Banner::Serious {
                areas: vec![],
                refused: false,
            },
            Banner::Retiring {
                since: None,
                ends: None,
            },
            Banner::TankLow { days: 1 },
            Banner::New { days: 1 },
            Banner::Sealed,
            Banner::Served,
        ];
        let ranks = ranks(&order);
        assert!(ranks.windows(2).all(|pair| pair[0] < pair[1]), "{ranks:?}");
    }

    #[test]
    fn an_empty_tank_outranks_serious_concerns_and_replaces_tank_low() {
        let mut strategy = quiet();
        strategy.vault.tank.balance = 0.0;
        if let Some(review) = strategy.review.as_mut() {
            review.ratings[0] = Rating::SeriousConcerns;
        }
        let shown = banners(&strategy);
        assert_eq!(shown[0], Banner::TankEmpty);
        assert!(matches!(shown[1], Banner::Serious { .. }));
        assert!(
            !all_banners(&strategy)
                .iter()
                .any(|b| matches!(b, Banner::TankLow { .. }))
        );
    }

    #[test]
    fn a_retiring_agent_names_the_moment_it_stops_and_a_frozen_one_outranks_an_empty_tank() {
        let mut retiring = quiet();
        retiring.vault.state = VaultState::WindingDown;
        retiring.vault.halt = Some(shared::Halt {
            at: time::macros::datetime!(2026-09-25 18:00),
            reason: "retired".into(),
        });
        let shown = banners(&retiring);
        assert!(matches!(
            shown[0],
            Banner::Retiring { ends: Some(end), .. } if end == time::macros::datetime!(2026-10-02 18:00)
        ));
        let mut frozen = quiet();
        frozen.vault.state = VaultState::Frozen;
        frozen.vault.tank.balance = 0.0;
        assert!(matches!(banners(&frozen)[0], Banner::Frozen { .. }));
    }

    #[test]
    fn a_killed_vault_leads_with_its_kill_and_a_closed_one_drops_the_running_overlays() {
        let mut killed = quiet();
        killed.vault.state = VaultState::Closed;
        killed.kill = Some(shared::Kill {
            at: time::macros::datetime!(2026-09-21 18:28),
            reason: "its health report showed repeated failed orders".into(),
            outcome: "the cash came home".into(),
            reference: "0x91b4".into(),
        });
        let shown = banners(&killed);
        assert!(matches!(shown[0], Banner::Killed { .. }), "{shown:?}");
        assert_eq!(shown[1], Banner::Closed);
        let mut closed = quiet();
        closed.vault.state = VaultState::Closed;
        closed.vault.tank.balance = 0.0;
        closed.bundle.source = Source::Sealed;
        assert_eq!(all_banners(&closed), vec![Banner::Closed]);
    }
}
