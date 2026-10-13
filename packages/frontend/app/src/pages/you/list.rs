//! `/you` ("Your agents"): every agent the connected account published and
//! every publishing job it paid for, so an Author can always get back to a
//! job and to an agent's controls.
//!
//! Everything listed is the snapshot's: each agent's author is the address
//! its vault takes the Author's controls from, and the jobs are every paid
//! job the build service lists (an upload waiting for its payment isn't
//! listed). A job is this account's when:
//!
//! - its agent is in the snapshot, by that agent's author;
//! - or the queue names this account's address as the wallet that paid;
//! - or the queue names the account by the name it shows this account by on
//!   the same chain (a handle, as the account's agents are shown), since the
//!   API shows each name for one author only;
//! - or this browser paid for it from this account (remembered locally, so
//!   a job just paid for shows before the queue lists it).
//!
//! An EVM address is compared by its bytes, so its case never matters; a
//! Solana key by its spelling.

use std::rc::Rc;

use futures_signals::signal::{Mutable, SignalExt};
use serde::{Deserialize, Serialize};
use shared::{Chain, ChainAddress, Job, JobState, Strategy, VaultState};
use time::PrimitiveDateTime;
use ui::icons::Icon;

use crate::atoms::badge::{Tone, badge};
use crate::atoms::button::{Button, Kind, on_click};
use crate::atoms::notice::{NoticeTone, notice};
use crate::components::wallet::open_connect;
use crate::pages::Page;
use crate::pages::agent::facts::{ChipTone, state_chip};
use crate::pages::publish::parts::this_year;
use crate::store::{Agent, Store};
use crate::view::{View, el, text};
use crate::web::app_link;

/// How many of the jobs this browser paid for it remembers.
pub const KEEP: usize = 5;
/// Where they are kept (`shonin.` is added in front).
const KEY: &str = "jobs";

/// The accounts connected now: at most one of each kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Accounts {
    pub evm: Option<ChainAddress>,
    pub solana: Option<ChainAddress>,
}

impl Accounts {
    pub fn of(
        evm: Option<alloy_primitives::Address>,
        solana: Option<chain::solana_ix::Pubkey>,
    ) -> Self {
        Self {
            evm: evm.map(|account| ChainAddress::Evm(account.0.0)),
            solana: solana.map(|account| ChainAddress::Solana(account.to_bytes())),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.evm.is_none() && self.solana.is_none()
    }

    /// Whether `address`, on `chain`, is one of these accounts: an EVM
    /// account on the EVM chains, a Solana one on Solana.
    pub fn holds(&self, chain: Chain, address: &ChainAddress) -> bool {
        let ours = match chain {
            Chain::Solana => self.solana.as_ref(),
            Chain::Base | Chain::Hyperliquid | Chain::Robinhood => self.evm.as_ref(),
        };
        ours == Some(address)
    }

    /// The same, for an address as text in either case.
    pub fn holds_text(&self, chain: Chain, address: &str) -> bool {
        ChainAddress::parse(address).is_ok_and(|address| self.holds(chain, &address))
    }
}

/// A job this browser paid for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    pub id: String,
    pub title: String,
    pub chain: Chain,
    /// The account that paid, as its chain writes it.
    pub account: String,
    /// When the payment landed, on this browser's clock: seconds since the
    /// Unix epoch.
    pub at: i64,
}

impl Remembered {
    fn when(&self) -> Option<PrimitiveDateTime> {
        let at = time::OffsetDateTime::from_unix_timestamp(self.at).ok()?;
        Some(PrimitiveDateTime::new(at.date(), at.time()))
    }
}

/// `list` with `job` added: newest first, one entry per job, at most
/// [`KEEP`].
pub fn kept(mut list: Vec<Remembered>, job: Remembered) -> Vec<Remembered> {
    list.retain(|held| held.id != job.id);
    list.insert(0, job);
    list.truncate(KEEP);
    list
}

thread_local! {
    /// What this browser remembers, read from storage the first time it is
    /// asked for.
    static REMEMBERED: Mutable<Option<Vec<Remembered>>> = Mutable::new(None);
}

/// The jobs this browser remembers paying for, read once from storage.
/// Storage is a convenience: anything unreadable is nothing remembered.
fn remembered() -> Mutable<Option<Vec<Remembered>>> {
    let held = REMEMBERED.with(Mutable::clone);
    if held.lock_ref().is_none() {
        held.set(Some(read_remembered()));
    }
    held
}

fn read_remembered() -> Vec<Remembered> {
    #[cfg(target_arch = "wasm32")]
    {
        crate::web::storage_get(KEY)
            .and_then(|text| serde_json::from_str::<Vec<Remembered>>(&text).ok())
            .map(|mut list| {
                list.truncate(KEEP);
                list
            })
            .unwrap_or_default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    Vec::new()
}

/// Remember a job this browser just paid for: its id, its title, its chain
/// and the account that paid. Shown on `/you` until the queue lists it.
pub fn remember(id: &str, title: &str, chain: Chain, account: &str) {
    let job = Remembered {
        id: id.to_string(),
        title: title.to_string(),
        chain,
        account: account.to_string(),
        at: (crate::web::now_ms() / 1_000.0) as i64,
    };
    let held = remembered();
    let list = kept(held.get_cloned().unwrap_or_default(), job);
    if let Ok(text) = serde_json::to_string(&list) {
        crate::web::storage_set(KEY, &text);
    }
    held.set(Some(list));
}

/// One row of the jobs list.
#[derive(Clone, Debug)]
pub enum JobRow {
    /// A job the queue lists, with where to activate its agent when it is
    /// ready and has no vault yet.
    Listed {
        job: Box<Job>,
        activate: Option<String>,
    },
    /// A job this browser paid for that the queue doesn't list yet.
    Remembered(Remembered),
}

impl JobRow {
    fn when(&self) -> Option<PrimitiveDateTime> {
        match self {
            Self::Listed { job, .. } => Some(job.submitted),
            Self::Remembered(job) => job.when(),
        }
    }
}

/// What the page lists for the accounts connected.
pub struct Listing {
    /// Whether two accounts are connected: the words say "these wallets".
    pub both: bool,
    pub agents: Vec<Rc<Agent>>,
    pub jobs: Vec<JobRow>,
    /// When the build service last listed its jobs, if its latest answer
    /// failed.
    pub stale: Option<Option<PrimitiveDateTime>>,
}

/// The agent a job published, by its slug: the one the queue gives, or
/// the one its id makes on its chain.
fn job_slug(job: &Job) -> Option<String> {
    job.strategy_slug
        .clone()
        .or_else(|| job.strategy_id.map(|id| Strategy::slug_for(job.chain, id)))
}

/// Whether a job is one of `accounts`' (see the module's notes).
pub fn job_is_yours(
    job: &Job,
    accounts: &Accounts,
    strategies: &[Rc<Strategy>],
    remembered: &[Remembered],
) -> bool {
    if remembered
        .iter()
        .any(|held| held.id == job.id && accounts.holds_text(held.chain, &held.account))
    {
        return true;
    }
    let slug = job_slug(job);
    if let Some(agent) = strategies
        .iter()
        .find(|strategy| Some(&strategy.slug) == slug.as_ref())
    {
        return accounts.holds_text(agent.vault.chain, &agent.author_address);
    }
    if let Ok(address) = ChainAddress::parse(&job.author) {
        return accounts.holds(job.chain, &address);
    }
    let name = job.author.trim();
    !name.is_empty()
        && strategies.iter().any(|strategy| {
            strategy.vault.chain == job.chain
                && strategy.author.trim() == name
                && accounts.holds_text(strategy.vault.chain, &strategy.author_address)
        })
}

/// Where to activate a job's agent: a ready job whose agent has no vault in
/// the snapshot yet.
pub fn activate_href(job: &Job, strategies: &[Rc<Strategy>]) -> Option<String> {
    if job.state != JobState::Ready {
        return None;
    }
    let slug = job_slug(job)?;
    (!strategies.iter().any(|strategy| strategy.slug == slug))
        .then(|| format!("/publish/{slug}/activate"))
}

/// Whether an agent has a vault its Author can manage.
pub fn has_vault(strategy: &Strategy) -> bool {
    !strategy.vault.address.trim().is_empty() && strategy.vault.state != VaultState::Cancelled
}

/// Everything listed for `accounts`, from the store as it is.
pub fn listing(accounts: &Accounts, store: &Store, remembered: &[Remembered]) -> Listing {
    let all = store.all();
    let strategies: Vec<Rc<Strategy>> = all
        .iter()
        .map(|agent| agent.strategy.get_cloned())
        .collect();
    let mut agents: Vec<Rc<Agent>> = all
        .into_iter()
        .filter(|agent| accounts.holds_text(agent.chain, &agent.strategy.lock_ref().author_address))
        .collect();
    agents.sort_by(|a, b| {
        b.registered
            .get()
            .cmp(&a.registered.get())
            .then_with(|| a.slug.cmp(&b.slug))
    });

    let listed = store.jobs.get_cloned();
    let mut jobs: Vec<JobRow> = listed
        .iter()
        .filter(|job| job_is_yours(job, accounts, &strategies, remembered))
        .map(|job| JobRow::Listed {
            job: Box::new(job.clone()),
            activate: activate_href(job, &strategies),
        })
        .collect();
    jobs.extend(
        remembered
            .iter()
            .filter(|held| accounts.holds_text(held.chain, &held.account))
            .filter(|held| !listed.iter().any(|job| job.id == held.id))
            .cloned()
            .map(JobRow::Remembered),
    );
    jobs.sort_by_key(|row| std::cmp::Reverse(row.when()));

    Listing {
        both: accounts.evm.is_some() && accounts.solana.is_some(),
        agents,
        jobs,
        stale: store.jobs_stale.get().then(|| store.jobs_as_of.get()),
    }
}

// The page ---------------------------------------------------------------------

pub fn page() -> Page {
    let session = crate::wallet::get();
    let solana = crate::wallet::solana::get();
    let store = crate::store::get();
    let accounts = futures_signals::map_ref! {
        let evm = session.connection.signal_cloned(),
        let solana = solana.connection.signal_cloned() => Accounts::of(evm.account(), solana.account())
    }
    .dedupe()
    .broadcast();
    let held = remembered();
    let content = futures_signals::map_ref! {
        let accounts = accounts.signal(),
        let loaded = store.loaded.signal(),
        let _members = store.membership.signal(),
        let _jobs = store.jobs.signal_cloned(),
        let _stale = store.jobs_stale.signal(),
        let remembered = held.signal_cloned() => {
            let store = crate::store::get();
            if accounts.is_empty() {
                signed_out()
            } else if !*loaded {
                loading()
            } else {
                listing_view(&listing(accounts, &store, remembered.as_deref().unwrap_or(&[])))
            }
        }
    };
    let lede = accounts.signal().map(|accounts| lede(&accounts));
    let body = el("div")
        .class("sh-author sh-you")
        .child(
            el("div")
                .class("sh-author__head")
                .child(
                    el("div")
                        .class("sh-author__headtext")
                        .child(el("h1").class("sh-author__title").text("Your agents"))
                        .child(
                            el("p")
                                .class("sh-author__lede")
                                .text_signal(lede_words(), lede),
                        ),
                )
                .child(
                    el("div").class("sh-author__links").child(
                        app_link("/publish")
                            .class("sh-btn sh-btn-secondary sh-btn-sm")
                            .text("Publish an agent"),
                    ),
                ),
        )
        .child_signal(Some(loading()), content.map(Some));
    Page {
        top: None,
        body: body.into(),
    }
}

fn lede_words() -> String {
    "The agents you’ve published and your publishing jobs.".to_string()
}

/// Under the title: which wallets the page is for.
fn lede(accounts: &Accounts) -> String {
    let short = |address: &ChainAddress| {
        crate::format::short_hash(&match address {
            ChainAddress::Evm(bytes) => format!("{:?}", alloy_primitives::Address::from(*bytes)),
            ChainAddress::Solana(_) => address.to_string(),
        })
    };
    match (&accounts.evm, &accounts.solana) {
        (Some(evm), Some(solana)) => format!(
            "The agents you’ve published and your publishing jobs, for {} and Solana {}.",
            short(evm),
            short(solana)
        ),
        (Some(evm), None) => format!(
            "The agents you’ve published and your publishing jobs, for {}.",
            short(evm)
        ),
        (None, Some(solana)) => format!(
            "The agents you’ve published and your publishing jobs, for Solana {}.",
            short(solana)
        ),
        (None, None) => lede_words(),
    }
}

fn signed_out() -> View {
    el("div")
        .class("sh-card sh-card--pad sh-you__gate")
        .child(el("p").text(
            "Connect the wallet you publish with to see the agents you’ve published and your publishing jobs.",
        ))
        .child(
            on_click(
                Button::new(Kind::Primary, "Connect wallet").build(),
                open_connect,
            )
            .attr("aria-haspopup", "dialog"),
        )
        .into()
}

fn loading() -> View {
    el("div")
        .class("sh-job__loading")
        .attr("aria-busy", "true")
        .child(crate::atoms::skeleton::skeleton_block("100%", 120))
        .child(crate::atoms::skeleton::skeleton_block("100%", 200))
        .into()
}

/// The two lists.
pub fn listing_view(listing: &Listing) -> View {
    let wallet = if listing.both {
        "these wallets"
    } else {
        "this wallet"
    };
    View::Many(vec![
        agents_section(listing, wallet),
        jobs_section(listing, wallet),
    ])
}

fn section_head(id: &'static str, title: &'static str, count: usize) -> View {
    el("div")
        .class("sh-you__sectionhead")
        .child(el("h2").attr("id", id).class("sh-you__h2").text(title))
        .child(if count > 0 {
            el("span")
                .class("sh-muted")
                .text(match count {
                    1 => "1 listed".to_string(),
                    count => format!("{count} listed"),
                })
                .into()
        } else {
            View::Empty
        })
        .into()
}

fn empty(line: String) -> View {
    el("div")
        .class("sh-card sh-card--pad sh-you__empty")
        .child(el("p").text(line))
        .child(
            app_link("/publish")
                .class("sh-link")
                .text("Publish an agent"),
        )
        .into()
}

fn agents_section(listing: &Listing, wallet: &str) -> View {
    let body = if listing.agents.is_empty() {
        empty(format!(
            "No agents from {wallet} yet. An agent is listed here once it’s activated."
        ))
    } else {
        el("ul")
            .class("sh-card sh-you__list")
            .children(listing.agents.iter().map(agent_row))
            .into()
    };
    el("section")
        .class("sh-you__section")
        .attr("aria-labelledby", "you-agents")
        .child(section_head("you-agents", "Agents", listing.agents.len()))
        .child(body)
        .into()
}

fn tone(chip: &ChipTone) -> Tone {
    match chip {
        ChipTone::Gain => Tone::Gain,
        ChipTone::Neutral => Tone::Neutral,
        ChipTone::Caution => Tone::Caution,
        ChipTone::Loss => Tone::Loss,
    }
}

fn agent_row(agent: &Rc<Agent>) -> View {
    let strategy = agent.strategy.get_cloned();
    let chain = agent.chain.label();
    let manage: View = if has_vault(&strategy) {
        app_link(format!("/you/agents/{}", agent.slug))
            .class("sh-btn sh-btn-secondary sh-btn-sm")
            .attr(
                "aria-label",
                format!("Manage {} on {chain}", agent.title.get_cloned()),
            )
            .text("Manage")
            .into()
    } else {
        View::Empty
    };
    el("li")
        .class("sh-you__row")
        .child(
            el("div")
                .class("sh-you__main")
                .child(
                    app_link(agent.href())
                        .class("sh-you__name")
                        .text_from(&agent.title, Clone::clone),
                )
                .child(
                    el("p")
                        .class("sh-you__meta")
                        .text(format!("{chain} · {}", agent.slug)),
                ),
        )
        .child(
            el("div")
                .class("sh-you__state")
                .child_from(&agent.strategy, |strategy| {
                    let (words, chip) = state_chip(strategy, crate::store::get().as_of.get());
                    Some(badge(tone(&chip), None, words).into())
                }),
        )
        .child(
            el("div")
                .class("sh-you__fig")
                .child(el("span").class("sh-label").text("In vault"))
                .child(
                    el("span")
                        .class("sh-fig")
                        .text_from(&agent.in_vault, |value| crate::format::money_whole(*value)),
                ),
        )
        .child(el("div").class("sh-you__actions").child(manage))
        .into()
}

fn jobs_section(listing: &Listing, wallet: &str) -> View {
    let stale: View = match listing.stale {
        Some(at) => {
            let when = at
                .map(|at| {
                    format!(
                        ", as of {}",
                        crate::format::day_time(at, this_year(at.year()))
                    )
                })
                .unwrap_or_default();
            notice(
                NoticeTone::Caution,
                text(format!(
                    "The build service isn’t answering right now, so these are the jobs as it last listed them{when}."
                )),
            )
            .attr("role", "status")
            .into()
        }
        None => View::Empty,
    };
    let body = if listing.jobs.is_empty() {
        empty(format!("No publishing jobs paid for from {wallet}."))
    } else {
        el("ul")
            .class("sh-card sh-you__list")
            .children(listing.jobs.iter().map(job_row))
            .into()
    };
    el("section")
        .class("sh-you__section")
        .attr("aria-labelledby", "you-jobs")
        .child(section_head("you-jobs", "Publishing jobs", listing.jobs.len()))
        .child(el("p").class("sh-muted sh-author__small").text(
            "Newest first. A job is listed once its payment is seen; this browser also keeps the last few it paid for.",
        ))
        .child(stale)
        .child(body)
        .into()
}

fn job_title(id: &str, title: &str) -> String {
    if title.trim().is_empty() {
        format!("Job {id}")
    } else {
        title.to_string()
    }
}

fn job_row(row: &JobRow) -> View {
    let (id, title, chain, meta, chip, action): (String, String, Chain, String, View, View) =
        match row {
            JobRow::Listed { job, activate } => {
                let submitted =
                    crate::format::day_time(job.submitted, this_year(job.submitted.year()));
                let action: View = match activate {
                    Some(href) => app_link(href.clone())
                        .class("sh-btn sh-btn-primary sh-btn-sm")
                        .attr(
                            "aria-label",
                            format!("Activate {}", job_title(&job.id, &job.title)),
                        )
                        .text("Activate")
                        .into(),
                    None => View::Empty,
                };
                (
                    job.id.clone(),
                    job_title(&job.id, &job.title),
                    job.chain,
                    format!("Submitted {submitted}"),
                    crate::pages::publish::job::chip(&job.state).into(),
                    action,
                )
            }
            JobRow::Remembered(held) => {
                let paid = held
                    .when()
                    .map(|at| {
                        format!(
                            "Paid {} from this browser · not in the queue’s list yet",
                            crate::format::day_time(at, this_year(at.year()))
                        )
                    })
                    .unwrap_or_else(|| {
                        "Paid from this browser · not in the queue’s list yet".to_string()
                    });
                (
                    held.id.clone(),
                    job_title(&held.id, &held.title),
                    held.chain,
                    paid,
                    badge(Tone::Neutral, Some(Icon::Clock), "Paid").into(),
                    View::Empty,
                )
            }
        };
    el("li")
        .class("sh-you__row sh-you__row--job")
        .child(
            el("div")
                .class("sh-you__main")
                .child(
                    app_link(format!("/publish/jobs/{}", crate::route::encode(&id)))
                        .class("sh-you__name")
                        .text(title),
                )
                .child(
                    el("p")
                        .class("sh-you__meta")
                        .text(format!("Job {id} · {} · {meta}", chain.label())),
                ),
        )
        .child(el("div").class("sh-you__state").child(chip))
        .child(el("div").class("sh-you__actions").child(action))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINE: &str = "0x7ed5Ac216DfE8c7F4FdF42Ea0650BACee007f703";
    const THEIRS: &str = "0xb0b79fde2e89a335b41abef4f349ba473714b448";

    fn snapshot() -> shared::Snapshot {
        serde_json::from_str(include_str!("../../../tests/snapshot.json"))
            .expect("the captured snapshot decodes")
    }

    fn mine() -> Accounts {
        Accounts {
            // In upper case: the snapshot writes it in lower case.
            evm: Some(ChainAddress::parse(&MINE.to_uppercase()).unwrap()),
            solana: None,
        }
    }

    fn job(id: &str, author: &str, state: JobState, strategy_id: Option<u64>) -> Job {
        Job {
            id: id.to_string(),
            chain: Chain::Base,
            strategy_id,
            title: format!("Agent of job {id}"),
            author: author.to_string(),
            state,
            submitted: time::macros::datetime!(2026-10-13 05:00)
                + time::Duration::minutes(id.parse::<i64>().unwrap() - 9000),
            duration_seconds: 0,
            source_hash: String::new(),
            sealed: false,
            bundle_hash: None,
            review: None,
            strategy_slug: None,
        }
    }

    /// The captured snapshot with its first two agents this account's (its
    /// address in lower case) and the rest someone else's, and jobs from
    /// both, named every way the queue names a wallet.
    fn store() -> Store {
        let mut snapshot = snapshot();
        for (at, strategy) in snapshot.strategies.iter_mut().enumerate() {
            let (address, name) = if at < 2 {
                (MINE.to_lowercase(), "atlas".to_string())
            } else {
                (THEIRS.to_string(), "someone".to_string())
            };
            strategy.author_address = address;
            strategy.author = name;
            strategy.title = format!("Agent {}", strategy.slug);
        }
        let first = snapshot.strategies[0].clone();
        let mut by_agent = job("9001", "atlas", JobState::Activated, Some(first.id));
        by_agent.strategy_slug = Some(first.slug.clone());
        snapshot.jobs = vec![
            by_agent,
            // Named by its address, in lower case.
            job("9002", &MINE.to_lowercase(), JobState::Ready, Some(41)),
            // Named by the handle its agents are shown by.
            job("9003", "atlas", JobState::Building, Some(42)),
            job("9004", THEIRS, JobState::Ready, Some(43)),
            job("9005", "someone", JobState::Refused, Some(44)),
            // A handle names this account only on the chain its agents are
            // shown by it on.
            job("9006", "atlas", JobState::Queued { position: 1 }, None),
        ];
        snapshot.jobs[5].chain = Chain::Hyperliquid;
        let store = Store::new();
        store.apply(&snapshot);
        store
    }

    #[test]
    fn the_page_lists_only_the_connected_accounts_agents_and_jobs_in_any_case() {
        let store = store();
        let listing = listing(&mine(), &store, &[]);
        let agents: Vec<&str> = listing
            .agents
            .iter()
            .map(|agent| agent.slug.as_str())
            .collect();
        let snapshot = snapshot();
        let mut expected: Vec<&str> = snapshot.strategies[..2]
            .iter()
            .map(|strategy| strategy.slug.as_str())
            .collect();
        expected.sort_by_key(|slug| {
            std::cmp::Reverse(
                snapshot
                    .strategies
                    .iter()
                    .find(|strategy| strategy.slug == *slug)
                    .unwrap()
                    .registered,
            )
        });
        assert_eq!(agents, expected);
        let jobs: Vec<String> = listing
            .jobs
            .iter()
            .map(|row| match row {
                JobRow::Listed { job, .. } => job.id.clone(),
                JobRow::Remembered(held) => held.id.clone(),
            })
            .collect();
        assert_eq!(jobs, ["9003", "9002", "9001"], "newest first, only ours");

        let html = listing_view(&listing).html();
        for theirs in &snapshot.strategies[2..] {
            assert!(
                !html.contains(&format!("/agents/{}\"", theirs.slug)),
                "{html}"
            );
        }
        for theirs in ["9004", "9005", "9006"] {
            assert!(!html.contains(&format!("/publish/jobs/{theirs}")), "{html}");
        }

        // The other account sees its own, and none of these.
        let theirs = Accounts {
            evm: Some(ChainAddress::parse(THEIRS).unwrap()),
            solana: None,
        };
        let listing = super::listing(&theirs, &store, &[]);
        assert_eq!(listing.agents.len(), snapshot.strategies.len() - 2);
        assert!(listing.jobs.iter().all(|row| matches!(
            row,
            JobRow::Listed { job, .. } if job.id == "9004" || job.id == "9005"
        )));

        // A Solana key never matches on an EVM chain.
        let solana = Accounts {
            evm: None,
            solana: Some(ChainAddress::Solana([7; 32])),
        };
        let listing = super::listing(&solana, &store, &[]);
        assert!(listing.agents.is_empty() && listing.jobs.is_empty());
    }

    #[test]
    fn a_ready_job_with_no_vault_yet_offers_activate() {
        let store = store();
        let listing = listing(&mine(), &store, &[]);
        let html = listing_view(&listing).html();
        assert!(
            html.contains("href=\"/publish/base-41/activate\""),
            "{html}"
        );
        assert!(html.contains(">Activate<"));
        // A building or activated job offers no activation.
        assert!(!html.contains("/publish/base-42/activate"));
        assert_eq!(html.matches("/activate\"").count(), 1);
        // Activated: its agent is listed with its controls instead.
        let first = &snapshot().strategies[0];
        assert!(html.contains(&format!("href=\"/you/agents/{}\"", first.slug)));

        // A job still said ready whose agent already has a vault is not
        // offered again.
        let mut ready = job("9010", MINE, JobState::Ready, Some(first.id));
        ready.strategy_slug = None;
        let strategies: Vec<Rc<Strategy>> = store
            .all()
            .iter()
            .map(|agent| agent.strategy.get_cloned())
            .collect();
        assert_eq!(activate_href(&ready, &strategies), None);
    }

    #[test]
    fn with_nothing_published_both_lists_say_so_and_point_to_publish() {
        let store = Store::new();
        store.apply(&snapshot());
        let nobody = Accounts {
            evm: Some(ChainAddress::Evm([0x11; 20])),
            solana: None,
        };
        let listing = listing(&nobody, &store, &[]);
        assert!(listing.agents.is_empty() && listing.jobs.is_empty());
        let html = listing_view(&listing).html();
        assert!(html.contains("No agents from this wallet yet."), "{html}");
        assert!(
            html.contains("No publishing jobs paid for from this wallet."),
            "{html}"
        );
        assert_eq!(html.matches("href=\"/publish\"").count(), 2, "{html}");
        assert!(!html.contains("sh-you__list"));
    }

    #[test]
    fn a_job_this_browser_paid_for_shows_before_the_queue_lists_it() {
        let store = store();
        let paid = Remembered {
            id: "9100".to_string(),
            title: "Spread keeper".to_string(),
            chain: Chain::Base,
            account: MINE.to_string(),
            at: time::macros::datetime!(2026-10-13 09:00)
                .assume_utc()
                .unix_timestamp(),
        };
        let listing = listing(&mine(), &store, std::slice::from_ref(&paid));
        assert!(matches!(&listing.jobs[0], JobRow::Remembered(held) if held.id == "9100"));
        let html = listing_view(&listing).html();
        assert!(html.contains("href=\"/publish/jobs/9100\""), "{html}");
        assert!(html.contains("Spread keeper"));
        assert!(html.contains("not in the queue’s list yet"));

        // Once listed, the queue's row is the one shown.
        let mut listed = store.jobs.get_cloned().to_vec();
        let mut job = job("9100", "someone else entirely", JobState::Reviewing, None);
        job.title = "Spread keeper".to_string();
        listed.push(job);
        store.jobs.set(Rc::new(listed));
        let listing = super::listing(&mine(), &store, std::slice::from_ref(&paid));
        let rows: Vec<_> = listing
            .jobs
            .iter()
            .filter(|row| match row {
                JobRow::Listed { job, .. } => job.id == "9100",
                JobRow::Remembered(held) => held.id == "9100",
            })
            .collect();
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], JobRow::Listed { .. }));

        // Remembered for another account: not shown.
        let theirs = Accounts {
            evm: Some(ChainAddress::parse(THEIRS).unwrap()),
            solana: None,
        };
        let listing = super::listing(
            &theirs,
            &store,
            &[Remembered {
                id: "9200".to_string(),
                ..paid
            }],
        );
        assert!(
            listing
                .jobs
                .iter()
                .all(|row| !matches!(row, JobRow::Remembered(_)))
        );
    }

    #[test]
    fn a_browser_keeps_only_the_last_few_jobs_once_each() {
        let at = |id: u32| Remembered {
            id: id.to_string(),
            title: String::new(),
            chain: Chain::Base,
            account: MINE.to_string(),
            at: i64::from(id),
        };
        let mut list = Vec::new();
        for id in 0..8 {
            list = kept(list, at(id));
        }
        list = kept(list, at(5));
        let ids: Vec<&str> = list.iter().map(|held| held.id.as_str()).collect();
        assert_eq!(ids, ["5", "7", "6", "4", "3"]);
        assert_eq!(list.len(), KEEP);
    }

    #[test]
    fn signed_out_it_asks_for_the_wallet_you_publish_with() {
        let html = signed_out().html();
        assert!(html.contains("Connect the wallet you publish with"));
        assert!(html.contains("Connect wallet"));
    }
}
