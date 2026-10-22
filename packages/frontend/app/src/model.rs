//! What the pages say about an agent, worked out from the snapshot: plain
//! functions with no web dependencies, so the pre-render, the app and the
//! tests agree.

use shared::{
    Chain, ChainAddress, ExitClass, Rating, Review, Source, Standing, Strategy, VaultState,
};
use time::{Duration, PrimitiveDateTime};

/// The tank gauge fills against this many days of running.
pub const GAUGE_DAYS: f64 = 60.0;
/// Under this many days of running left, the tank is low.
pub const LOW_TANK_DAYS: f64 = 7.0;
/// A retiring agent keeps running this long after its author retires it.
pub const WIND_DOWN_DAYS: i64 = 7;

/// Which list on the leaderboard an agent belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    /// Live, with 30 days of history: ranked.
    Open,
    /// Live, with less than 30 days: listed, not ranked.
    New,
    /// Retiring, frozen, killed or closed: listed so its record stays visible.
    Closing,
    /// Activating or cancelled: never listed.
    Hidden,
}

impl Group {
    pub fn of(standing: Standing, state: VaultState) -> Group {
        match state {
            VaultState::Activating | VaultState::Cancelled => return Group::Hidden,
            VaultState::WindingDown
            | VaultState::Frozen
            | VaultState::Killed
            | VaultState::Closed => return Group::Closing,
            VaultState::Open => {}
        }
        match standing {
            Standing::Listed => Group::Open,
            Standing::New => Group::New,
            Standing::Retiring | Standing::Frozen | Standing::Killed | Standing::Closed => {
                Group::Closing
            }
        }
    }
}

/// The four ratings, most severe first (the rating summary rule: Serious ›
/// Some › Couldn't tell › Looks fine).
pub const SEVERITY: [Rating; 4] = [
    Rating::SeriousConcerns,
    Rating::SomeConcerns,
    Rating::CouldntTell,
    Rating::LooksFine,
];

/// How a rating reads: exactly these four strings, always with their icon.
pub fn rating_word(rating: Rating) -> &'static str {
    match rating {
        Rating::LooksFine => "Looks fine",
        Rating::SomeConcerns => "Some concerns",
        Rating::SeriousConcerns => "Serious concerns",
        Rating::CouldntTell => "Couldn’t tell",
    }
}

/// A review boiled down for a row: the most severe rating present and how
/// many of the six areas have it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewSummary {
    /// Every area looks fine.
    AllFine,
    /// The most severe rating present, and how many areas have it.
    Worst { rating: Rating, areas: usize },
    /// The reviewer refused the code as written to do harm.
    Refused,
    /// No review has been posted.
    None,
}

impl ReviewSummary {
    pub fn of(review: Option<&Review>) -> Self {
        let Some(review) = review else {
            return Self::None;
        };
        if review.refused {
            return Self::Refused;
        }
        for rating in SEVERITY {
            if rating == Rating::LooksFine {
                break;
            }
            let areas = review.ratings.iter().filter(|r| **r == rating).count();
            if areas > 0 {
                return Self::Worst { rating, areas };
            }
        }
        Self::AllFine
    }

    /// The chip's words: "Looks fine", "2 areas: serious concerns".
    pub fn words(&self) -> String {
        match self {
            Self::AllFine => "Looks fine".to_string(),
            Self::Worst { rating, areas } => format!(
                "{areas} area{}: {}",
                if *areas == 1 { "" } else { "s" },
                rating_word(*rating).to_lowercase()
            ),
            Self::Refused => "Refused".to_string(),
            Self::None => "No review yet".to_string(),
        }
    }

    /// The rating whose icon and colour the chip carries.
    pub fn rating(&self) -> Option<Rating> {
        match self {
            Self::AllFine => Some(Rating::LooksFine),
            Self::Worst { rating, .. } => Some(*rating),
            Self::Refused => Some(Rating::SeriousConcerns),
            Self::None => None,
        }
    }

    pub fn has_serious(&self) -> bool {
        matches!(
            self,
            Self::Refused
                | Self::Worst {
                    rating: Rating::SeriousConcerns,
                    ..
                }
        )
    }
}

/// The facts under an agent's name: "Solana · Served exit · Source sealed".
/// Source sealed and Served exit are facts, never warning chips.
pub fn meta_line(strategy: &Strategy) -> String {
    let mut parts = vec![strategy.vault.chain.label().to_string()];
    parts.push(
        match strategy.vault.exit_class {
            ExitClass::Direct => "Direct exit",
            ExitClass::Served { .. } => "Served exit",
        }
        .to_string(),
    );
    if !strategy.bundle.models.is_empty() {
        parts.push("AI model".to_string());
    }
    if matches!(strategy.bundle.source, Source::Sealed) {
        parts.push("Source sealed".to_string());
    }
    parts.join(" · ")
}

/// The record's figures, or none for a vault with no record yet.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Figures {
    /// Over the last 30 days, once there are 30 days of history.
    pub return_30d: Option<f64>,
    pub since_opening: Option<f64>,
    pub deepest_drop: Option<f64>,
    /// Days of history behind the figures.
    pub days: u32,
}

/// What the vault holds at its last price: its shares outstanding at the
/// share price it last set. The recorded value is its gross at that
/// valuation, taken before any deposit or withdrawal settled since, so a
/// vault that took $25,000 after its last valuation still records what it
/// held before; its shares, priced at that valuation, already count it.
/// Without shares or a price, the recorded value is all there is.
pub fn in_vault(strategy: &Strategy) -> f64 {
    let vault = &strategy.vault;
    let priced = vault.shares * vault.share_price;
    if vault.shares > 0.0 && vault.share_price > 0.0 && priced.is_finite() {
        priced
    } else {
        vault.value
    }
}

pub fn figures(strategy: &Strategy) -> Figures {
    match &strategy.record {
        Some(record) if !record.value_per_share.is_empty() => {
            let performance = record.performance();
            Figures {
                return_30d: performance.return_30d,
                since_opening: Some(performance.return_since_open),
                deepest_drop: Some(performance.deepest_drop),
                days: performance.days.max(strategy.days_live),
            }
        }
        _ => Figures {
            days: strategy.days_live,
            ..Figures::default()
        },
    }
}

/// The last 30 days of the share price, for a sparkline.
pub fn spark(strategy: &Strategy) -> Vec<f64> {
    let Some(record) = &strategy.record else {
        return Vec::new();
    };
    let series = &record.value_per_share;
    let start = series.len().saturating_sub(31);
    series[start..].iter().map(|point| point.value).collect()
}

/// An SVG path for a sparkline in a `width` x `height` box, one point per
/// day, scaled to the series' own range. Fewer than two points draws nothing.
pub fn spark_path(values: &[f64], width: f64, height: f64) -> Option<String> {
    if values.len() < 2 || values.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let low = values.iter().copied().fold(f64::INFINITY, f64::min);
    let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = high - low;
    let pad = 2.0;
    let step = width / (values.len() - 1) as f64;
    let y = |value: f64| {
        if span <= f64::EPSILON {
            height / 2.0
        } else {
            pad + (high - value) / span * (height - 2.0 * pad)
        }
    };
    let mut path = String::new();
    for (index, value) in values.iter().enumerate() {
        let command = if index == 0 { 'M' } else { 'L' };
        path.push_str(&format!(
            "{command}{:.1} {:.1}",
            index as f64 * step,
            y(*value)
        ));
    }
    Some(path)
}

/// How the tank stands: days of running left at recent use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TankState {
    pub balance: f64,
    /// Days of running left; infinite when nothing is being charged.
    pub days: f64,
}

impl TankState {
    pub fn of(strategy: &Strategy) -> Self {
        let tank = &strategy.vault.tank;
        Self {
            balance: tank.balance,
            days: if tank.balance <= 0.0 {
                0.0
            } else {
                tank.runway_days()
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        self.balance <= 0.0
    }

    pub fn is_low(&self) -> bool {
        !self.is_empty() && self.days < LOW_TANK_DAYS
    }

    /// How full the gauge is drawn, from 0 to 1, against [`GAUGE_DAYS`].
    pub fn fill(&self) -> f64 {
        if !self.days.is_finite() {
            return 1.0;
        }
        (self.days / GAUGE_DAYS).clamp(0.0, 1.0)
    }

    /// The runway in a few words, for a table cell: "41 days", "Empty".
    pub fn short(&self) -> String {
        if self.is_empty() {
            "Empty".to_string()
        } else if !self.days.is_finite() {
            "No charges yet".to_string()
        } else if self.days < 1.0 {
            "Under a day".to_string()
        } else {
            crate::format::days(self.days.floor() as u32)
        }
    }

    /// The runway as a sentence, which is what the gauge means.
    pub fn sentence(&self) -> String {
        if self.is_empty() {
            "Stopped: tank empty".to_string()
        } else if !self.days.is_finite() {
            "Nothing has been charged to the tank yet.".to_string()
        } else {
            format!(
                "{} of running left at recent use.",
                crate::format::runway(self.days)
            )
        }
    }
}

/// When a retiring agent stops for good: seven days after its author retired
/// it.
pub fn wind_down_ends(strategy: &Strategy) -> Option<PrimitiveDateTime> {
    let halt = strategy.vault.halt.as_ref()?;
    (strategy.vault.state == VaultState::WindingDown)
        .then(|| halt.at + Duration::days(WIND_DOWN_DAYS))
}

/// Whole days left until `end`, rounded up, as of `now`.
pub fn days_until(end: PrimitiveDateTime, now: PrimitiveDateTime) -> i64 {
    let seconds = (end - now).whole_seconds();
    if seconds <= 0 {
        0
    } else {
        (seconds + 86_399) / 86_400
    }
}

/// A vault the indexer has never read whole: marked stale as of the Unix
/// epoch, with no figures to show. It is counted, never shown as an agent.
pub fn is_unread(strategy: &Strategy) -> bool {
    strategy
        .stale_as_of
        .is_some_and(|at| at.date() == time::OffsetDateTime::UNIX_EPOCH.date())
}

/// An author, the way a link names one: `<chain>:<address>`, URL-safe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorKey {
    pub chain: Chain,
    pub address: ChainAddress,
}

impl AuthorKey {
    pub fn parse(text: &str) -> Option<Self> {
        let (chain, address) = text.split_once(':')?;
        Some(Self {
            chain: Chain::from_name(chain)?,
            address: ChainAddress::parse(address).ok()?,
        })
    }

    pub fn of(strategy: &Strategy) -> Option<Self> {
        Some(Self {
            chain: strategy.vault.chain,
            address: ChainAddress::parse(&strategy.author_address).ok()?,
        })
    }

    /// Whether an agent is this author's: the same chain and the same
    /// account (an EVM address in any case).
    pub fn matches(&self, strategy: &Strategy) -> bool {
        Self::of(strategy).as_ref() == Some(self)
    }

    pub fn key(&self) -> String {
        format!("{}:{}", self.chain.name(), self.address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review(ratings: [Rating; 6]) -> Review {
        Review {
            ratings,
            findings: vec![],
            summary: String::new(),
            reviewed: time::Date::from_calendar_date(2026, time::Month::October, 1).unwrap(),
            refused: false,
            sealed: false,
            reviewer_image: String::new(),
            model_id_hash: String::new(),
            prompt_hash: String::new(),
            report_hash: String::new(),
            limits: vec![],
            recorded_tx: None,
        }
    }

    use Rating::{CouldntTell as T, LooksFine as F, SeriousConcerns as S, SomeConcerns as C};

    /// A vault that took deposits after its last valuation records its value
    /// from before them; what it holds at its last price is its shares at
    /// that price. Seen on the testnet: $25,000 in, a recorded value of a
    /// cent, 24,975 shares at $0.999999.
    #[test]
    fn what_a_vault_holds_is_its_shares_at_its_last_price() {
        let snapshot: shared::Snapshot =
            serde_json::from_str(include_str!("../tests/snapshot.json")).unwrap();
        let mut strategy = snapshot.strategies[0].clone();
        strategy.vault.value = 0.01;
        strategy.vault.shares = 24_975.0;
        strategy.vault.share_price = 0.999_999;
        assert!((in_vault(&strategy) - 24_974.975_025).abs() < 0.001);
        strategy.vault.shares = 0.0;
        assert_eq!(in_vault(&strategy), 0.01);
    }

    #[test]
    fn the_summary_shows_the_most_severe_rating_present_and_how_many_areas_have_it() {
        let summary = ReviewSummary::of(Some(&review([F, C, S, C, S, F])));
        assert_eq!(summary.words(), "2 areas: serious concerns");
        assert_eq!(summary.rating(), Some(S));
        assert!(summary.has_serious());
        assert_eq!(
            ReviewSummary::of(Some(&review([F, C, F, F, F, F]))).words(),
            "1 area: some concerns"
        );
    }

    #[test]
    fn couldnt_tell_ranks_below_some_concerns_and_above_looks_fine() {
        assert_eq!(
            ReviewSummary::of(Some(&review([T, C, T, F, F, F]))).words(),
            "1 area: some concerns"
        );
        assert_eq!(
            ReviewSummary::of(Some(&review([F, T, T, T, T, T]))).words(),
            "5 areas: couldn’t tell"
        );
    }

    #[test]
    fn six_fine_areas_read_just_looks_fine() {
        let summary = ReviewSummary::of(Some(&review([F; 6])));
        assert_eq!(summary.words(), "Looks fine");
        assert!(!summary.has_serious());
    }

    #[test]
    fn the_four_ratings_are_exactly_the_four_strings() {
        let words: Vec<&str> = SEVERITY.iter().map(|r| rating_word(*r)).collect();
        assert_eq!(
            words,
            [
                "Serious concerns",
                "Some concerns",
                "Couldn’t tell",
                "Looks fine"
            ]
        );
    }

    #[test]
    fn a_sparkline_spans_its_box_and_needs_two_points() {
        assert_eq!(spark_path(&[1.0], 96.0, 28.0), None);
        let path = spark_path(&[1.0, 2.0, 1.5], 96.0, 28.0).unwrap();
        assert!(path.starts_with("M0.0 26.0"));
        assert!(path.contains("L48.0 2.0"));
        assert!(path.ends_with("L96.0 14.0"));
        let flat = spark_path(&[1.0, 1.0], 96.0, 28.0).unwrap();
        assert_eq!(flat, "M0.0 14.0L96.0 14.0");
    }

    #[test]
    fn days_left_round_up_to_whole_days() {
        let now = time::macros::datetime!(2026-09-29 12:00);
        assert_eq!(
            days_until(time::macros::datetime!(2026-10-02 18:00), now),
            4
        );
        assert_eq!(
            days_until(time::macros::datetime!(2026-09-29 12:00), now),
            0
        );
        assert_eq!(
            days_until(time::macros::datetime!(2026-09-29 12:00:01), now),
            1
        );
    }

    #[test]
    fn an_author_key_matches_an_evm_address_in_any_case_but_only_on_its_chain() {
        let key = AuthorKey::parse("base:0x07CC6A64A539792BB53FD29E3118BF6D17A1F31A").unwrap();
        assert_eq!(key.key(), "base:0x07cc6a64a539792bb53fd29e3118bf6d17a1f31a");
        assert!(AuthorKey::parse("nowhere:0x07cc6a64a539792bb53fd29e3118bf6d17a1f31a").is_none());
        assert!(AuthorKey::parse("base:not-an-address").is_none());
        assert!(AuthorKey::parse("base").is_none());
    }

    #[test]
    fn a_group_follows_the_vault_before_the_standing() {
        assert_eq!(Group::of(Standing::Listed, VaultState::Open), Group::Open);
        assert_eq!(Group::of(Standing::New, VaultState::Open), Group::New);
        assert_eq!(
            Group::of(Standing::Listed, VaultState::WindingDown),
            Group::Closing
        );
        assert_eq!(
            Group::of(Standing::New, VaultState::Activating),
            Group::Hidden
        );
        assert_eq!(
            Group::of(Standing::Closed, VaultState::Cancelled),
            Group::Hidden
        );
    }
}
