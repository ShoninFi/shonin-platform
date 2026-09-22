//! The data the app renders, and where it comes from.
//!
//! Every figure on the screen is read from the chain by the platform's indexer
//! and handed to the app as one snapshot, so a page never shows two numbers
//! from two different moments. The app holds the latest snapshot, asks for a
//! newer one every couple of seconds, and re-renders when it gets one: a
//! record ticks, a build moves down the queue and a claim settles without
//! anyone reloading.
//!
//! Nothing here is generated. If the platform has nothing on it, the app says
//! so rather than showing something made up.

use std::cell::RefCell;
use std::rc::Rc;

use futures_signals::signal::{Mutable, Signal, SignalExt};
use shared::model::registry::{Author, FeedEvent, Job};
use shared::model::strategy::Strategy;
use shared::record;
use shared::{Benchmark, ChainInfo, Config, Performance, Point, Snapshot, Totals};
use time::{Date, PrimitiveDateTime};

/// Everything the app knows, as of one moment on the chain.
pub struct Dataset {
    pub version: u64,
    /// The chain's own clock. Everything dated is relative to it, so a page
    /// reads correctly however fast the chain is running.
    pub as_of: PrimitiveDateTime,
    pub chain: ChainInfo,
    pub strategies: Vec<Rc<Strategy>>,
    pub authors: Vec<Rc<Author>>,
    pub jobs: Vec<Rc<Job>>,
    pub feed: Vec<Rc<FeedEvent>>,
    pub benchmarks: Vec<Benchmark>,
    pub totals: Totals,
}

impl From<Snapshot> for Dataset {
    fn from(snapshot: Snapshot) -> Self {
        Self {
            version: snapshot.version,
            as_of: snapshot.as_of,
            chain: snapshot.chain,
            strategies: snapshot.strategies.into_iter().map(Rc::new).collect(),
            authors: snapshot.authors.into_iter().map(Rc::new).collect(),
            jobs: snapshot.jobs.into_iter().map(Rc::new).collect(),
            feed: snapshot.feed.into_iter().map(Rc::new).collect(),
            benchmarks: snapshot.benchmarks,
            totals: snapshot.totals,
        }
    }
}

/// What the app is showing now, and how it got there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Loading {
    /// Nothing has arrived yet.
    Waiting,
    Ready,
    /// The API could not be reached, with what it said.
    Failed(String),
}

thread_local! {
    static DATASET: Mutable<Option<Rc<Dataset>>> = Mutable::new(None);
    static LOADING: Mutable<Loading> = Mutable::new(Loading::Waiting);
    static CONFIG: RefCell<Option<Rc<Config>>> = const { RefCell::new(None) };
}

/// The snapshot the app is rendering, if one has arrived.
pub fn current() -> Option<Rc<Dataset>> {
    DATASET.with(|dataset| dataset.get_cloned())
}

/// A signal that fires whenever a newer snapshot arrives.
pub fn changes() -> impl Signal<Item = u64> {
    DATASET.with(|dataset| {
        dataset
            .signal_ref(|held| held.as_ref().map(|dataset| dataset.version).unwrap_or(0))
            .dedupe()
    })
}

pub fn loading() -> impl Signal<Item = Loading> {
    LOADING.with(|loading| loading.signal_cloned())
}

pub fn loading_now() -> Loading {
    LOADING.with(|loading| loading.get_cloned())
}

/// Take a snapshot as the app's current view of the platform.
pub fn set(snapshot: Snapshot) {
    let dataset = Rc::new(Dataset::from(snapshot));
    DATASET.with(|held| held.set(Some(dataset)));
    LOADING.with(|loading| loading.set(Loading::Ready));
}

pub fn set_failed(why: impl Into<String>) {
    LOADING.with(|loading| loading.set(Loading::Failed(why.into())));
}

/// How the app reaches the chain. Fetched once, beside the first snapshot.
pub fn config() -> Option<Rc<Config>> {
    CONFIG.with(|config| config.borrow().clone())
}

pub fn set_config(config: Config) {
    CONFIG.with(|held| *held.borrow_mut() = Some(Rc::new(config)));
}

// What a page asks for ------------------------------------------------------

/// The chain's clock. Before anything has arrived, the app has no clock of its
/// own to offer, so it uses the epoch and shows nothing dated.
pub fn now() -> PrimitiveDateTime {
    current()
        .map(|dataset| dataset.as_of)
        .unwrap_or_else(|| PrimitiveDateTime::new(Date::MIN, time::Time::MIDNIGHT))
}

pub fn as_of() -> Date {
    now().date()
}

pub fn vaults() -> Vec<Rc<Strategy>> {
    current()
        .map(|dataset| dataset.strategies.clone())
        .unwrap_or_default()
}

pub fn strategy(slug: &str) -> Option<Rc<Strategy>> {
    current()?
        .strategies
        .iter()
        .find(|strategy| strategy.slug == slug)
        .cloned()
}

pub fn author(handle: &str) -> Option<Rc<Author>> {
    current()?
        .authors
        .iter()
        .find(|author| author.handle.eq_ignore_ascii_case(handle))
        .cloned()
}

/// Everything an author has published, newest first.
pub fn by_author(handle: &str) -> Vec<Rc<Strategy>> {
    let mut found: Vec<Rc<Strategy>> = vaults()
        .into_iter()
        .filter(|strategy| strategy.author.eq_ignore_ascii_case(handle))
        .collect();
    found.sort_by_key(|strategy| std::cmp::Reverse(strategy.registered));
    found
}

pub fn authors() -> Vec<Rc<Author>> {
    current()
        .map(|dataset| dataset.authors.clone())
        .unwrap_or_default()
}

pub fn jobs() -> Vec<Rc<Job>> {
    current()
        .map(|dataset| dataset.jobs.clone())
        .unwrap_or_default()
}

pub fn job(id: &str) -> Option<Rc<Job>> {
    current()?.jobs.iter().find(|job| job.id == id).cloned()
}

pub fn feed() -> Vec<Rc<FeedEvent>> {
    current()
        .map(|dataset| dataset.feed.clone())
        .unwrap_or_default()
}

pub fn benchmarks() -> Vec<Benchmark> {
    current()
        .map(|dataset| dataset.benchmarks.clone())
        .unwrap_or_default()
}

/// A strategy's record, summarised. Computed from the series the chain gave
/// it, by the same maths the indexer uses.
pub fn performance(strategy: &Strategy) -> Performance {
    strategy
        .record
        .as_ref()
        .map(|held| held.performance())
        .unwrap_or(Performance {
            raw_annualised: 0.0,
            normalised_annualised: 0.0,
            volatility: 0.0,
            max_drawdown: 0.0,
            total_return: 0.0,
            days: 0,
        })
}

/// How two records moved together.
pub fn correlation(left: &[Point], right: &[Point]) -> f64 {
    record::correlation(left, right)
}

/// Every live vault with enough history to be ranked, best normalised return
/// first.
pub fn ranked() -> Vec<Rc<Strategy>> {
    let mut live: Vec<Rc<Strategy>> = vaults()
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
pub fn newest() -> Vec<Rc<Strategy>> {
    let mut new: Vec<Rc<Strategy>> = vaults()
        .into_iter()
        .filter(|strategy| strategy.standing == shared::Standing::New)
        .collect();
    new.sort_by_key(|strategy| strategy.days_live);
    new
}

/// Vaults that are no longer taking a record forward.
pub fn past() -> Vec<Rc<Strategy>> {
    vaults()
        .into_iter()
        .filter(|strategy| !strategy.standing.is_live())
        .collect()
}

pub fn total_vault_capital() -> f64 {
    current()
        .map(|dataset| dataset.totals.vault_capital)
        .unwrap_or(0.0)
}

pub fn total_trades() -> u32 {
    current().map(|dataset| dataset.totals.trades).unwrap_or(0)
}

/// A day this many days before the chain's today.
pub fn days_ago(days: u32) -> Date {
    as_of() - time::Duration::days(i64::from(days))
}

/// A moment this many hours before the chain's now.
pub fn hours_ago(hours: u32) -> PrimitiveDateTime {
    now() - time::Duration::hours(i64::from(hours))
}
