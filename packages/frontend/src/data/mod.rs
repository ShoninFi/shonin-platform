//! The data the app renders.
//!
//! Every strategy, vault, review, component, build and feed entry in the app
//! is built here, once, from seeded generators: nothing is fetched, and the
//! same dataset is built by the app in the browser and by the build-time
//! pre-render, so a page reads the same before and after it boots.

pub mod authors;
pub mod components;
pub mod feed;
pub mod queue;
pub mod series;
pub mod source;
pub mod strategies;

use std::sync::OnceLock;

use shared::{Author, Component, FeedEvent, Job, Strategy};
use time::{Date, Month, PrimitiveDateTime};

use series::Rng;

/// The day the dataset is read as of. Everything dated is relative to it, so
/// records end today and nothing drifts out of date between builds.
pub fn as_of() -> Date {
    Date::from_calendar_date(2026, Month::September, 20).expect("a real date")
}

/// The same moment, with a time of day, for anything stamped to the minute.
pub fn now() -> PrimitiveDateTime {
    as_of().with_hms(14, 20, 0).expect("a real time")
}

/// A day this many days before today.
pub fn days_ago(days: u32) -> Date {
    as_of() - time::Duration::days(i64::from(days))
}

/// A moment this many hours before now.
pub fn hours_ago(hours: u32) -> PrimitiveDateTime {
    now() - time::Duration::hours(i64::from(hours))
}

/// Everything the app knows.
pub struct Dataset {
    pub strategies: Vec<Strategy>,
    pub authors: Vec<Author>,
    pub components: Vec<Component>,
    pub jobs: Vec<Job>,
    pub feed: Vec<FeedEvent>,
}

static DATASET: OnceLock<Dataset> = OnceLock::new();

/// The dataset, built on first use.
pub fn get() -> &'static Dataset {
    DATASET.get_or_init(|| Dataset {
        strategies: strategies::all(),
        authors: authors::all(),
        components: components::all(),
        jobs: queue::all(),
        feed: feed::all(),
    })
}

// ------------------------------------------------------------------ lookups

/// Every strategy that has a vault.
pub fn vaults() -> Vec<&'static Strategy> {
    get()
        .strategies
        .iter()
        .filter(|strategy| !strategy.is_connected())
        .collect()
}

/// Every strategy published for owners' own broker accounts.
pub fn agents() -> Vec<&'static Strategy> {
    get()
        .strategies
        .iter()
        .filter(|strategy| strategy.is_connected())
        .collect()
}

pub fn strategy(slug: &str) -> Option<&'static Strategy> {
    get()
        .strategies
        .iter()
        .find(|strategy| strategy.slug == slug)
}

pub fn author(handle: &str) -> Option<&'static Author> {
    get().authors.iter().find(|author| author.handle == handle)
}

/// Everything an author has ever published, newest first.
pub fn by_author(handle: &str) -> Vec<&'static Strategy> {
    let mut found: Vec<&'static Strategy> = get()
        .strategies
        .iter()
        .filter(|strategy| strategy.author == handle)
        .collect();
    found.sort_by_key(|strategy| std::cmp::Reverse(strategy.registered));
    found
}

pub fn component(slug: &str) -> Option<&'static Component> {
    get()
        .components
        .iter()
        .find(|component| component.slug == slug)
}

/// The strategies that compose a component.
pub fn composing(slug: &str) -> Vec<&'static Strategy> {
    get()
        .strategies
        .iter()
        .filter(|strategy| {
            strategy
                .bundle
                .components
                .iter()
                .any(|pin| pin.slug == slug)
        })
        .collect()
}

pub fn job(id: &str) -> Option<&'static Job> {
    get().jobs.iter().find(|job| job.id == id)
}

/// A strategy's record, summarised. Both the app and the pre-render read it
/// from here, so a figure is computed once however it is shown.
pub fn performance(strategy: &Strategy) -> shared::Performance {
    strategy.record.as_ref().map_or(
        shared::Performance {
            raw_annualised: 0.0,
            normalised_annualised: 0.0,
            volatility: 0.0,
            max_drawdown: 0.0,
            total_return: 0.0,
            days: 0,
        },
        |record| series::performance(&record.value_per_share, &record.normalised),
    )
}

/// Every live vault strategy, best normalised return first.
pub fn ranked() -> Vec<&'static Strategy> {
    let mut live: Vec<&'static Strategy> = vaults()
        .into_iter()
        .filter(|strategy| strategy.standing == shared::Standing::Ranked)
        .collect();
    live.sort_by(|a, b| {
        performance(b)
            .normalised_annualised
            .partial_cmp(&performance(a).normalised_annualised)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    live
}

/// Live vaults too new to be ranked, newest first.
pub fn newest() -> Vec<&'static Strategy> {
    let mut new: Vec<&'static Strategy> = vaults()
        .into_iter()
        .filter(|strategy| strategy.standing == shared::Standing::New)
        .collect();
    new.sort_by_key(|strategy| strategy.days_live);
    new
}

/// Vaults that are no longer taking a record forward.
pub fn past() -> Vec<&'static Strategy> {
    vaults()
        .into_iter()
        .filter(|strategy| !strategy.standing.is_live())
        .collect()
}

/// Simple things to compare a record against, so a depositor can see whether
/// a strategy is adding anything to what they already hold.
pub fn benchmarks() -> &'static [(&'static str, Vec<shared::Point>)] {
    static BENCHMARKS: OnceLock<Vec<(&'static str, Vec<shared::Point>)>> = OnceLock::new();
    BENCHMARKS.get_or_init(|| {
        vec![
            (
                "BTC",
                series::record(
                    "benchmark-btc",
                    as_of(),
                    series::Shape::new(400, 0.34, 0.52, 1.0, 1.0),
                )
                .value_per_share,
            ),
            (
                "ETH",
                series::record(
                    "benchmark-eth",
                    as_of(),
                    series::Shape::new(400, 0.21, 0.66, 1.0, 1.0),
                )
                .value_per_share,
            ),
            (
                "A basket of funding rates",
                series::record(
                    "benchmark-funding",
                    as_of(),
                    series::Shape::new(400, 0.11, 0.06, 1.0, 1.0),
                )
                .value_per_share,
            ),
        ]
    })
}

/// Capital across every live vault.
pub fn total_vault_capital() -> f64 {
    vaults()
        .iter()
        .filter(|strategy| strategy.standing.is_live())
        .map(|strategy| strategy.capital())
        .sum()
}

/// Capital across every connected account.
pub fn total_connected_capital() -> f64 {
    agents().iter().map(|strategy| strategy.capital()).sum()
}

/// Trades the platform has made, summed from the records.
pub fn total_trades() -> u32 {
    get()
        .strategies
        .iter()
        .filter_map(|strategy| strategy.record.as_ref())
        .map(|record| record.trades)
        .sum()
}

// ------------------------------------------------------ identifiers, seeded

/// A hex string of `length` characters, seeded so it never changes.
pub fn hex(seed: &str, length: usize) -> String {
    let mut rng = Rng::seeded(seed);
    let digits = b"0123456789abcdef";
    (0..length)
        .map(|_| char::from(digits[rng.int(0, 15) as usize]))
        .collect()
}

/// A content hash, as the registries store one.
pub fn sha256(seed: &str) -> String {
    format!("sha256:{}", hex(seed, 64))
}

/// An account, key or contract address.
pub fn address(seed: &str) -> String {
    format!("0x{}", hex(seed, 40))
}

/// A transaction reference.
pub fn tx(seed: &str) -> String {
    format!("0x{}", hex(seed, 64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{Product, Standing};

    #[test]
    fn slugs_are_unique_and_url_shaped() {
        let mut seen = Vec::new();
        for strategy in &get().strategies {
            assert!(
                !seen.contains(&strategy.slug),
                "two strategies share the slug {}",
                strategy.slug
            );
            assert!(
                strategy
                    .slug
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{} isn't a URL-safe slug",
                strategy.slug
            );
            seen.push(strategy.slug.clone());
        }
    }

    #[test]
    fn every_strategy_has_an_author_with_a_page() {
        for strategy in &get().strategies {
            assert!(
                author(&strategy.author).is_some(),
                "{} is by {}, who has no page",
                strategy.slug,
                strategy.author
            );
        }
    }

    #[test]
    fn every_pinned_component_is_in_the_registry() {
        for strategy in &get().strategies {
            for pin in &strategy.bundle.components {
                assert!(
                    component(&pin.slug).is_some(),
                    "{} pins {}, which isn't in the registry",
                    strategy.slug,
                    pin.slug
                );
            }
        }
    }

    /// A successor and its predecessor must point at each other, or the
    /// "new version available" banner would lead nowhere.
    #[test]
    fn lineage_links_resolve() {
        for strategy in &get().strategies {
            if let Some(successor) = &strategy.superseded_by {
                assert!(
                    self::strategy(successor).is_some(),
                    "{} says {successor} supersedes it, and there's no such strategy",
                    strategy.slug
                );
            }
        }
    }

    #[test]
    fn a_vault_is_never_worth_more_than_its_venues_hold() {
        for strategy in vaults() {
            let share: f64 = strategy.legs.iter().map(|leg| leg.share_of_value).sum();
            assert!(
                share <= 1.0,
                "{} puts {share} of itself on venues",
                strategy.slug
            );
        }
    }

    #[test]
    fn only_live_strategies_with_ninety_days_are_ranked() {
        for strategy in &get().strategies {
            if strategy.standing == Standing::Ranked {
                assert!(
                    strategy.days_live >= shared::Strategy::DAYS_TO_RANK,
                    "{} is ranked after {} days",
                    strategy.slug,
                    strategy.days_live
                );
            }
            if strategy.standing == Standing::New {
                assert!(strategy.days_live < shared::Strategy::DAYS_TO_RANK);
            }
        }
    }

    #[test]
    fn a_vaults_numbers_agree_with_its_record() {
        for strategy in vaults() {
            let Product::Vault(vault) = &strategy.product else {
                continue;
            };
            if strategy.standing == Standing::BuildFailed {
                continue;
            }
            let record = strategy.record.as_ref().expect("a live vault has a record");
            let last = record.value_per_share.last().unwrap().value;
            assert!(
                (vault.share_price - last).abs() < 1e-9,
                "{}'s share price doesn't match its record",
                strategy.slug
            );
            assert!(
                vault.high_water_mark >= vault.share_price - 1e-9,
                "{}'s high-water mark is below its share price",
                strategy.slug
            );
            assert!(vault.idle <= vault.value + 1e-9);
        }
    }

    #[test]
    fn the_dataset_covers_every_state_the_app_has_to_show() {
        let standings: Vec<Standing> = get()
            .strategies
            .iter()
            .map(|strategy| strategy.standing)
            .collect();
        for wanted in [
            Standing::Ranked,
            Standing::New,
            Standing::Superseded,
            Standing::Retired,
            Standing::Killed,
            Standing::BuildFailed,
        ] {
            assert!(standings.contains(&wanted), "nothing is {wanted:?}");
        }

        assert!(!agents().is_empty(), "no connected-account agents");
        assert!(
            vaults()
                .iter()
                .any(|strategy| !strategy.bundle.source.is_published()),
            "no sealed-source strategy"
        );
        assert!(
            vaults()
                .iter()
                .any(|strategy| !strategy.bundle.models.is_empty()),
            "no strategy with a model"
        );
        assert!(
            vaults().iter().any(|strategy| strategy
                .vault()
                .is_some_and(|vault| !vault.claims.is_empty())),
            "no vault with an outstanding claim"
        );
        assert!(
            get()
                .jobs
                .iter()
                .any(|job| matches!(job.state, shared::JobState::Failed { .. })),
            "no failed build in the queue"
        );
    }
}

#[cfg(test)]
mod figures {
    use super::*;

    /// Normalising rescales a record; it should not invent a different sign or
    /// a wildly different size from the record it rescales.
    #[test]
    fn normalising_keeps_a_record_recognisable() {
        for strategy in &get().strategies {
            if strategy.record.is_none() || strategy.days_live < 60 {
                continue;
            }
            let performance = performance(strategy);
            let raw = performance.raw_annualised;
            let normalised = performance.normalised_annualised;
            assert_eq!(
                raw > 0.0,
                normalised > 0.0,
                "{}: raw {raw:.3} and normalised {normalised:.3} disagree on sign",
                strategy.slug
            );
            assert!(
                normalised.abs() <= raw.abs() * 3.5 + 0.02,
                "{}: normalised {normalised:.3} is far past 3x raw {raw:.3}",
                strategy.slug
            );
        }
    }
}
