//! The one store: the latest snapshot, diffed into keyed state.
//!
//! Each agent is an [`Agent`] holding one `Mutable` per field a page shows,
//! updated with `set_neq`, so a new snapshot changes only the text nodes whose
//! values changed. Pages bind to these; no page is rebuilt because data
//! arrived. Which agents exist, and which list each is in, is tracked apart
//! ([`Store::membership`]): a list re-sorts only when that changes or when
//! someone asks, never because a figure moved.
//!
//! No web dependencies: the pre-render fills the same store from one snapshot,
//! and the tests drive it natively.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use futures_signals::signal::Mutable;
use futures_signals::signal_map::MutableBTreeMap;
use futures_signals::signal_vec::MutableVec;
use shared::{Chain, Config, FeedEvent, FeedKind, Snapshot, Standing, Strategy, VaultState};
use time::{Date, PrimitiveDateTime};

use crate::model::{self, AuthorKey, Figures, Group, ReviewSummary, TankState};

/// One agent, field by field.
pub struct Agent {
    pub slug: String,
    pub chain: Chain,
    pub title: Mutable<String>,
    /// The author's handle, as the registry names them.
    pub author: Mutable<String>,
    /// The author as a link names them: `<chain>:<address>`.
    pub author_key: Mutable<Option<String>>,
    /// "Hyperliquid · Direct exit · AI model"
    pub meta: Mutable<String>,
    pub served: Mutable<bool>,
    pub group: Mutable<Group>,
    pub state: Mutable<VaultState>,
    pub standing: Mutable<Standing>,
    pub review: Mutable<ReviewSummary>,
    pub figures: Mutable<Figures>,
    /// Everything in the vault, less the tank, in dollars.
    pub in_vault: Mutable<f64>,
    /// What deposits and withdrawals are paid in: USDC, USDG.
    pub asset: Mutable<String>,
    pub tank: Mutable<TankState>,
    /// The last 30 days of the share price.
    pub spark: Mutable<Vec<f64>>,
    /// Set when the indexer's latest read of this vault failed part-way: the
    /// figures are then as of this moment, and shown as such.
    pub stale_as_of: Mutable<Option<PrimitiveDateTime>>,
    /// When a retiring agent stops for good.
    pub wind_down_ends: Mutable<Option<PrimitiveDateTime>>,
    pub registered: Mutable<Date>,
    /// The whole record, for the agent page's detail (its review, venues,
    /// chart and activity). Replaced with each snapshot; pages derive what
    /// they show from it and redraw only what changed.
    pub strategy: Mutable<Rc<Strategy>>,
}

impl Agent {
    fn new(strategy: &Strategy) -> Self {
        let agent = Self {
            slug: strategy.slug.clone(),
            chain: strategy.vault.chain,
            title: Mutable::new(String::new()),
            author: Mutable::new(String::new()),
            author_key: Mutable::new(None),
            meta: Mutable::new(String::new()),
            served: Mutable::new(false),
            group: Mutable::new(Group::Hidden),
            state: Mutable::new(strategy.vault.state),
            standing: Mutable::new(strategy.standing),
            review: Mutable::new(ReviewSummary::None),
            figures: Mutable::new(Figures::default()),
            in_vault: Mutable::new(0.0),
            asset: Mutable::new(String::new()),
            tank: Mutable::new(TankState {
                balance: 0.0,
                days: 0.0,
            }),
            spark: Mutable::new(Vec::new()),
            stale_as_of: Mutable::new(None),
            wind_down_ends: Mutable::new(None),
            registered: Mutable::new(strategy.registered),
            strategy: Mutable::new(Rc::new(strategy.clone())),
        };
        agent.update(strategy);
        agent
    }

    /// Bring every field up to date, touching only those that changed.
    /// Answers whether the agent moved to another list.
    fn update(&self, strategy: &Strategy) -> bool {
        self.title.set_neq(strategy.title.clone());
        self.author.set_neq(strategy.author.clone());
        self.author_key
            .set_neq(AuthorKey::of(strategy).map(|key| key.key()));
        self.meta.set_neq(model::meta_line(strategy));
        self.served.set_neq(matches!(
            strategy.vault.exit_class,
            shared::ExitClass::Served { .. }
        ));
        self.state.set_neq(strategy.vault.state);
        self.standing.set_neq(strategy.standing);
        self.review
            .set_neq(ReviewSummary::of(strategy.review.as_ref()));
        self.figures.set_neq(model::figures(strategy));
        self.in_vault.set_neq(strategy.vault.value);
        self.asset.set_neq(strategy.vault.asset.clone());
        self.tank.set_neq(TankState::of(strategy));
        self.spark.set_neq(model::spark(strategy));
        self.stale_as_of.set_neq(strategy.stale_as_of);
        self.wind_down_ends.set_neq(model::wind_down_ends(strategy));
        self.registered.set_neq(strategy.registered);
        self.strategy.set(Rc::new(strategy.clone()));

        let group = Group::of(strategy.standing, strategy.vault.state);
        let moved = self.group.get() != group;
        self.group.set_neq(group);
        moved
    }

    /// The agent's page.
    pub fn href(&self) -> String {
        format!("/agents/{}", self.slug)
    }
}

/// One event on the public feed. Events never change once posted.
#[derive(Clone, Debug, PartialEq)]
pub struct FeedItem {
    pub at: PrimitiveDateTime,
    pub kind: FeedKind,
    pub title: String,
    pub detail: String,
    pub link: Option<String>,
    pub reference: String,
}

impl From<&FeedEvent> for FeedItem {
    fn from(event: &FeedEvent) -> Self {
        Self {
            at: event.at,
            kind: event.kind,
            title: event.title.clone(),
            detail: event.detail.clone(),
            link: event.link.clone(),
            reference: event.reference.clone(),
        }
    }
}

/// How fresh one vault chain's part of the snapshot is.
#[derive(Clone, Debug, PartialEq)]
pub struct ChainRead {
    pub chain: Chain,
    /// Which network it is: `evm:84532`, `solana:devnet`.
    pub key: String,
    pub block: u64,
    pub as_of: Option<PrimitiveDateTime>,
}

/// How the app is getting on with the API.
#[derive(Default)]
pub struct Sync {
    /// When the latest snapshot arrived, in milliseconds since the Unix epoch
    /// on the reader's clock. None until the first one has.
    pub last_ok: Mutable<Option<f64>>,
    /// When the app started asking, on the same clock.
    pub started: Mutable<f64>,
    /// Whether the latest ask failed.
    pub failing: Mutable<bool>,
    /// What the latest failure was, in words.
    pub error: Mutable<Option<String>>,
}

/// The store.
#[derive(Default)]
pub struct Store {
    pub agents: MutableBTreeMap<String, Rc<Agent>>,
    /// Rises whenever an agent joins, leaves or moves list.
    pub membership: Mutable<u64>,
    /// Vaults the indexer hasn't read whole yet: counted, not shown.
    pub unread: Mutable<usize>,
    /// The public feed, newest first.
    pub feed: MutableVec<Rc<FeedItem>>,
    pub chains: Mutable<Vec<ChainRead>>,
    /// The chains' own clock: the latest of them. None before the first
    /// snapshot.
    pub as_of: Mutable<Option<PrimitiveDateTime>>,
    /// How long a proposed machine image waits before it counts, read from
    /// the chain's terms.
    pub image_timelock_days: Mutable<Option<f64>>,
    pub sync: Sync,
    /// Whether the first snapshot has arrived.
    pub loaded: Mutable<bool>,
    version: Cell<Option<u64>>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a new snapshot in. A snapshot no newer than the one held changes
    /// nothing. Answers whether anything was applied.
    pub fn apply(&self, snapshot: &Snapshot) -> bool {
        if self.version.get() == Some(snapshot.version) {
            return false;
        }
        self.version.set(Some(snapshot.version));
        self.loaded.set_neq(true);
        self.as_of.set_neq(Some(snapshot.as_of));
        self.chains.set_neq(
            snapshot
                .chains
                .iter()
                .map(|info| ChainRead {
                    chain: info.chain,
                    key: info.key.clone(),
                    block: info.block,
                    as_of: info.as_of,
                })
                .collect(),
        );

        let mut moved = false;
        let mut unread = 0;
        let mut seen = BTreeSet::new();
        {
            let mut agents = self.agents.lock_mut();
            for strategy in &snapshot.strategies {
                if model::is_unread(strategy) {
                    unread += 1;
                    continue;
                }
                seen.insert(strategy.slug.clone());
                match agents.get(&strategy.slug) {
                    Some(agent) => moved |= agent.update(strategy),
                    None => {
                        agents.insert_cloned(strategy.slug.clone(), Rc::new(Agent::new(strategy)));
                        moved = true;
                    }
                }
            }
            let gone: Vec<String> = agents
                .keys()
                .filter(|slug| !seen.contains(*slug))
                .cloned()
                .collect();
            for slug in gone {
                agents.remove(&slug);
                moved = true;
            }
        }
        self.unread.set_neq(unread);
        if moved {
            self.membership.replace_with(|version| *version + 1);
        }

        let mut feed: Vec<FeedItem> = snapshot.feed.iter().map(FeedItem::from).collect();
        feed.sort_by_key(|item| std::cmp::Reverse(item.at));
        let changed = {
            let held = self.feed.lock_ref();
            held.len() != feed.len() || held.iter().zip(&feed).any(|(a, b)| **a != *b)
        };
        if changed {
            self.feed
                .lock_mut()
                .replace_cloned(feed.into_iter().map(Rc::new).collect());
        }
        true
    }

    /// Take a chain's configuration in: its terms are what the status page
    /// quotes.
    pub fn apply_config(&self, config: &Config) {
        self.image_timelock_days
            .set_neq(Some(config.terms.image_timelock_days));
    }

    /// Every agent shown, in slug order.
    pub fn all(&self) -> Vec<Rc<Agent>> {
        self.agents.lock_ref().values().cloned().collect()
    }

    pub fn agent(&self, slug: &str) -> Option<Rc<Agent>> {
        self.agents.lock_ref().get(slug).cloned()
    }

    /// Agents per list, for the tab counts.
    pub fn counts(&self) -> BTreeMap<Group, usize> {
        let mut counts = BTreeMap::new();
        for agent in self.agents.lock_ref().values() {
            *counts.entry(agent.group.get()).or_insert(0) += 1;
        }
        counts
    }

    /// Whether any snapshot has arrived.
    pub fn is_loaded(&self) -> bool {
        self.version.get().is_some()
    }
}

thread_local! {
    static STORE: Rc<Store> = Rc::new(Store::new());
}

/// The app's store. One per thread: the browser has one, and so does the
/// pre-render.
pub fn get() -> Rc<Store> {
    STORE.with(Rc::clone)
}
