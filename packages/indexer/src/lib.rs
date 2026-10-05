//! The reference indexer.
//!
//! It follows every vault chain and turns each vault's value per share into a
//! record, holding one snapshot that carries everything a page renders, so
//! every figure on the screen comes from the same moment. It needs no enclave and
//! holds no key: anyone can run their own and compare.
//!
//! The binary in this crate only reads. The web app's API runs the same
//! indexer in-process and serves what it holds.

mod logs;
pub mod read;
pub mod store;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use anyhow::Result;
use chain::Served;
use chain::bindings::attestation::{IAttestationRegistry, ImageKind};
use chain::bindings::vault::IVault;
use shared::Chain;
use shared::model::health::Health;
use shared::model::registry::{Author, FeedEvent, Job, JobState};
use shared::{ChainInfo, HealthPost, Snapshot, Totals};
use tokio::sync::RwLock;

use crate::read::History;
use crate::store::Store;

/// What became of a health report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthVerdict {
    Taken,
    /// Not a vault address, or not a signature.
    Malformed,
    /// No vault chain of that name is read here.
    UnknownChain,
    /// The vault chain is read here, and is not connected yet.
    NotConnected,
    /// The chain could not be asked.
    Unavailable,
    /// Signed by a key the vault has not blessed.
    NotBlessed,
    /// No newer than the report already held.
    Stale,
    /// Dated further ahead than any clock could drift.
    FromTheFuture,
}

impl HealthVerdict {
    /// The answer an HTTP route gives for it.
    pub fn status(self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self {
            Self::Taken => StatusCode::ACCEPTED,
            Self::Malformed => StatusCode::BAD_REQUEST,
            Self::UnknownChain => StatusCode::NOT_FOUND,
            Self::NotConnected => StatusCode::SERVICE_UNAVAILABLE,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::NotBlessed => StatusCode::FORBIDDEN,
            Self::Stale => StatusCode::CONFLICT,
            Self::FromTheFuture => StatusCode::UNPROCESSABLE_ENTITY,
        }
    }
}

/// How far ahead of this server's clock a report may be dated: clocks drift,
/// but not by much.
const CLOCK_SKEW: time::Duration = time::Duration::minutes(5);

/// Whether a report is dated too far ahead to be believed. One dated far
/// ahead would stand as the newest for as long as that, and no honest report
/// could replace it.
fn from_the_future(updated: time::PrimitiveDateTime, now: time::OffsetDateTime) -> bool {
    updated.assume_utc() > now + CLOCK_SKEW
}

/// Who signed a report, from the signature it carries.
fn recover(chain: Chain, post: &HealthPost) -> Result<Address> {
    use alloy::primitives::{B256, Signature};

    let bytes = alloy::hex::decode(&post.signature)?;
    let signature = Signature::try_from(bytes.as_slice())
        .map_err(|error| anyhow::anyhow!("the signature is malformed: {error}"))?;
    let digest = B256::from(HealthPost::digest(chain, &post.vault, &post.report));
    Ok(signature.recover_address_from_msg(digest.as_slice())?)
}

/// One vault chain, as the indexer reads it.
pub struct Reader {
    pub served: Served,
    history: RwLock<History>,
    /// When it connected, and when it was last read whole: what its health
    /// is judged by.
    connected: Instant,
    read_at: std::sync::Mutex<Option<Instant>>,
    /// Whether the last read of it found every vault on it stale: kept as
    /// last read whole, none read whole now. Such a read is not a read of
    /// the chain, and moves `read_at` on no further.
    every_vault_stale: std::sync::atomic::AtomicBool,
}

/// What one chain read came to: that chain's part of the snapshot.
#[derive(Clone)]
struct Read {
    info: ChainInfo,
    now: u64,
    strategies: Vec<shared::Strategy>,
    /// Every trade its vaults' stations have made, not only those shown.
    trades: u32,
    /// Everyone who has published on it, by address.
    authors: Vec<(Address, Author)>,
    feed: Vec<FeedEvent>,
}

/// The build queue as last read: the jobs the build service last gave, when
/// it gave them, and whether it has failed to answer since.
#[derive(Clone, Default)]
struct Queue {
    jobs: Vec<Job>,
    /// The wallet each job names, by chain and job id, as the build service
    /// gave it: who the job's author is, whatever they are called.
    wallets: HashMap<(Chain, String), Address>,
    as_of: Option<time::PrimitiveDateTime>,
    stale: bool,
}

/// One job as its chain last completed it, and when.
struct Completion {
    job: Job,
    at: Instant,
}

/// Where a vault chain stands with this indexer.
#[derive(Clone)]
pub enum Lookup {
    /// Connected, and read.
    Served(Box<Served>),
    /// To be read here, and not connected yet.
    NotConnected,
    /// Not read here at all.
    NotServed,
}

/// The record, as read from every vault chain, and what it needs to keep
/// reading.
pub struct Indexer {
    /// The vault chains to be read, connected or not yet: a chain is read
    /// from the moment it connects, and one that hasn't holds up no other.
    expected: Vec<Chain>,
    readers: std::sync::RwLock<Vec<Arc<Reader>>>,
    pub store: Store,
    /// Where the build service answers, for the queue. Without one the record
    /// is the chains alone and the queue reads as empty.
    pub builder: Option<String>,
    /// For asking other services, bounded so one that stops answering holds
    /// nothing up for good.
    pub http: reqwest::Client,
    /// Each chain's part of the record and the queue, each refreshed on its
    /// own: the snapshot is made from the latest of each, so one slow chain
    /// (or a slow build service) holds back no other part.
    parts: RwLock<HashMap<Chain, Read>>,
    queue: RwLock<Queue>,
    /// What each job's chain added to it, by chain and job id: kept from one
    /// refresh to the next, so a chain too slow to complete a whole queue at
    /// once still completes it a part at a time.
    completions: std::sync::Mutex<HashMap<(Chain, String), Completion>>,
    pub snapshot: RwLock<Snapshot>,
    /// Health reports, as executors post them, by vault chain and vault: the
    /// same address can be a different vault on another chain.
    pub health: RwLock<HashMap<(Chain, Address), Health>>,
    /// What each author is shown as, by vault chain and address, as the last
    /// snapshot had it (see [`shown_names`]).
    names: std::sync::RwLock<HashMap<(Chain, Address), String>>,
}

impl Indexer {
    /// An indexer for `expected`, reading none of them until each is
    /// handed over by [`Indexer::add`].
    pub fn new(expected: Vec<Chain>, store: PathBuf, builder: Option<String>) -> Self {
        Self {
            snapshot: RwLock::new(empty_snapshot()),
            expected,
            readers: std::sync::RwLock::new(Vec::new()),
            health: RwLock::new(HashMap::new()),
            store: Store::new(store),
            builder,
            http: http(),
            parts: RwLock::new(HashMap::new()),
            queue: RwLock::new(Queue::default()),
            completions: std::sync::Mutex::new(HashMap::new()),
            names: std::sync::RwLock::new(HashMap::new()),
        }
    }

    /// Start reading a chain that has connected.
    pub async fn add(&self, served: Served) {
        tracing::info!(chain = %served.key, "the indexer is reading");
        let info = ChainInfo {
            chain: served.vault_chain,
            chain_id: served.client.chain_id,
            block: 0,
            as_of: None,
        };
        {
            let mut readers = self.readers.write().unwrap_or_else(|e| e.into_inner());
            if readers
                .iter()
                .any(|reader| reader.served.vault_chain == served.vault_chain)
            {
                return;
            }
            readers.push(Arc::new(Reader {
                served,
                history: RwLock::new(History::default()),
                connected: Instant::now(),
                read_at: std::sync::Mutex::new(None),
                every_vault_stale: std::sync::atomic::AtomicBool::new(false),
            }));
        }
        let mut snapshot = self.snapshot.write().await;
        if snapshot.chain(info.chain).is_none() {
            snapshot.chains.push(info);
        }
    }

    /// Read each chain from the moment it is handed over, for as long as
    /// chains keep coming.
    pub async fn add_each(&self, mut connected: tokio::sync::mpsc::UnboundedReceiver<Served>) {
        while let Some(served) = connected.recv().await {
            self.add(served).await;
        }
    }

    /// Every chain read so far.
    pub fn readers(&self) -> Vec<Arc<Reader>> {
        self.readers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The chain a vault chain is read from, once it has connected.
    pub fn on(&self, chain: Chain) -> Option<Served> {
        self.readers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|reader| &reader.served)
            .find(|served| served.vault_chain == chain)
            .cloned()
    }

    /// Whether a vault chain is read here, not connected yet, or not read
    /// here at all.
    pub fn lookup(&self, chain: Chain) -> Lookup {
        match self.on(chain) {
            Some(served) => Lookup::Served(Box::new(served)),
            None if self.expected.contains(&chain) => Lookup::NotConnected,
            None => Lookup::NotServed,
        }
    }

    /// Take a health report if the key that signed it is blessed for the
    /// vault on this chain, and it is newer than the one already held. Only
    /// the instance running a strategy can write about it, and nobody can
    /// put an older report back.
    pub async fn take_health(&self, chain: Chain, post: HealthPost) -> HealthVerdict {
        let Ok(vault) = post.vault.parse::<Address>() else {
            return HealthVerdict::Malformed;
        };
        let Ok(signer) = recover(chain, &post) else {
            return HealthVerdict::Malformed;
        };
        if from_the_future(post.report.updated, time::OffsetDateTime::now_utc()) {
            return HealthVerdict::FromTheFuture;
        }
        let served = match self.lookup(chain) {
            Lookup::Served(served) => *served,
            Lookup::NotConnected => return HealthVerdict::NotConnected,
            Lookup::NotServed => return HealthVerdict::UnknownChain,
        };
        let bundle = match served.client.read(vault, IVault::bundleHashCall {}).await {
            Ok(bundle) => bundle,
            Err(error) => {
                tracing::warn!(%vault, %error, "could not read the vault for a health report");
                return HealthVerdict::Unavailable;
            }
        };
        let blessed = match served
            .client
            .read(
                served.deployments.attestation,
                IAttestationRegistry::isBlessedForCall {
                    signer,
                    vaultChain: alloy::primitives::U256::from(served.deployments.chain_id),
                    vault,
                    strategyHash: bundle,
                },
            )
            .await
        {
            Ok(blessed) => blessed,
            Err(error) => {
                tracing::warn!(%vault, %error, "could not read the registry for a health report");
                return HealthVerdict::Unavailable;
            }
        };
        // Blessed for the vault, and as an executor: only the instance that
        // runs the strategy speaks for how it is running.
        let executor = match served
            .client
            .read(
                served.deployments.attestation,
                IAttestationRegistry::keyInfoCall { signer },
            )
            .await
        {
            Ok(info) => matches!(info.kind, ImageKind::Executor),
            Err(error) => {
                tracing::warn!(%vault, %error, "could not read the registry for a health report");
                return HealthVerdict::Unavailable;
            }
        };
        if !blessed || !executor {
            tracing::warn!(%vault, %signer, "a health report from a key this vault has not blessed as its executor");
            return HealthVerdict::NotBlessed;
        }

        let mut health = self.health.write().await;
        if health
            .get(&(chain, vault))
            .is_some_and(|held| held.updated >= post.report.updated)
        {
            return HealthVerdict::Stale;
        }
        health.insert((chain, vault), post.report);
        HealthVerdict::Taken
    }

    /// Keep reading, every couple of seconds, for as long as the process
    /// runs: each chain, and the queue, on a loop of its own, so a slow one
    /// holds back no other. A chain is taken up as soon as it connects.
    pub async fn run(&self) {
        use futures::StreamExt;

        let mut running =
            futures::stream::FuturesUnordered::<futures::future::BoxFuture<'_, ()>>::new();
        running.push(Box::pin(async {
            loop {
                self.refresh_queue().await;
                tokio::time::sleep(PAUSE).await;
            }
        }));
        let mut started = std::collections::HashSet::new();
        loop {
            for reader in self.readers() {
                if started.insert(reader.served.vault_chain) {
                    running.push(Box::pin(async move {
                        loop {
                            self.refresh_chain(&reader).await;
                            tokio::time::sleep(PAUSE).await;
                        }
                    }));
                }
            }
            tokio::select! {
                _ = running.next() => {}
                _ = tokio::time::sleep(PAUSE) => {}
            }
        }
    }

    /// Read everything again, once: every chain and the queue together, each
    /// part written into the snapshot as soon as it is in. It is cheap enough
    /// to do often and simple enough to be obviously correct, which matters
    /// more here than being clever.
    pub async fn refresh(&self) -> Result<()> {
        let readers = self.readers();
        let chains =
            futures::future::join_all(readers.iter().map(|reader| self.refresh_chain(reader)));
        futures::join!(chains, self.refresh_queue());
        Ok(())
    }

    /// Read one chain again and put its part of the snapshot in place. A
    /// chain that can't be read keeps its last part, dated as it was. A read
    /// that found every vault on it stale is put in place too, each vault
    /// marked, but is not taken for a read of the chain: its health says so
    /// once that has lasted (see [`Indexer::health_at`]).
    async fn refresh_chain(&self, reader: &Reader) {
        let health = self.health.read().await.clone();
        match self.read_one(reader, &health).await {
            Ok(read) => {
                let every_vault_stale = !read.strategies.is_empty()
                    && read
                        .strategies
                        .iter()
                        .all(|strategy| strategy.stale_as_of.is_some());
                reader
                    .every_vault_stale
                    .store(every_vault_stale, std::sync::atomic::Ordering::SeqCst);
                self.parts
                    .write()
                    .await
                    .insert(reader.served.vault_chain, read);
                if !every_vault_stale {
                    *reader.read_at.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(Instant::now());
                }
                self.compose().await;
            }
            Err(error) => {
                tracing::warn!(chain = %reader.served.key, %error, "could not read this chain");
            }
        }
    }

    /// Ask the build service for its queue again and put it in place. One
    /// that can't be asked leaves the queue it last gave, marked stale and
    /// dated as it was, as a chain that can't be read leaves its part.
    async fn refresh_queue(&self) {
        match self.jobs().await {
            Ok((jobs, wallets)) => {
                let as_of = self.builder.as_ref().map(|_| {
                    let now = time::OffsetDateTime::now_utc();
                    time::PrimitiveDateTime::new(now.date(), now.time())
                });
                *self.queue.write().await = Queue {
                    jobs,
                    wallets,
                    as_of,
                    stale: false,
                };
            }
            Err(error) => {
                tracing::debug!(%error, "could not read the build queue");
                self.queue.write().await.stale = true;
            }
        }
        self.compose().await;
    }

    /// The snapshot, made again from the latest part of each chain and the
    /// latest queue.
    async fn compose(&self) {
        let readers = self.readers();
        let parts = self.parts.read().await;
        let mut queue = self.queue.read().await.clone();

        let mut infos = Vec::new();
        let mut latest = 0;
        let mut strategies: Vec<shared::Strategy> = Vec::new();
        let mut authors: Vec<((Chain, Address), Author)> = Vec::new();
        let mut feed = Vec::new();
        let mut trades = 0;
        for reader in &readers {
            let Some(read) = parts.get(&reader.served.vault_chain) else {
                // Connected, and not read yet.
                infos.push(ChainInfo {
                    chain: reader.served.vault_chain,
                    chain_id: reader.served.client.chain_id,
                    block: 0,
                    as_of: None,
                });
                continue;
            };
            infos.push(read.info.clone());
            latest = latest.max(read.now);
            strategies.extend(read.strategies.iter().cloned());
            trades += read.trades;
            feed.extend(read.feed.iter().cloned());
            // An author is an address on a chain: two who share a handle are
            // never one.
            for (address, author) in &read.authors {
                let key = (reader.served.vault_chain, *address);
                if !authors.iter().any(|(known, _)| *known == key) {
                    authors.push((key, author.clone()));
                }
            }
        }
        drop(parts);
        feed.sort_by_key(|event| std::cmp::Reverse(event.at));

        // What each author is called, the handle they set (or their address)
        // as each place read it: their profile first, then their vaults, then
        // their jobs.
        let mut handles: Vec<((Chain, Address), String)> = authors
            .iter()
            .map(|(key, author)| (*key, author.handle.clone()))
            .collect();
        for strategy in &strategies {
            if let Ok(address) = strategy.author_address.parse::<Address>() {
                handles.push(((strategy.vault.chain, address), strategy.author.clone()));
            }
        }
        for job in &queue.jobs {
            if let Some(address) = queue.wallets.get(&(job.chain, job.id.clone())) {
                handles.push(((job.chain, *address), job.author.clone()));
            }
        }
        let names = shown_names(&handles);
        for strategy in &mut strategies {
            if let Some(name) = strategy
                .author_address
                .parse::<Address>()
                .ok()
                .and_then(|address| names.get(&(strategy.vault.chain, address)))
            {
                strategy.author = name.clone();
            }
        }
        for job in &mut queue.jobs {
            if let Some(name) = queue
                .wallets
                .get(&(job.chain, job.id.clone()))
                .and_then(|address| names.get(&(job.chain, *address)))
            {
                job.author = name.clone();
            }
        }
        let authors: Vec<Author> = authors
            .into_iter()
            .map(|(key, author)| Author {
                handle: names.get(&key).cloned().unwrap_or(author.handle),
                ..author
            })
            .collect();
        *self.names.write().unwrap_or_else(|e| e.into_inner()) = names;

        let totals = Totals {
            // Every vault still holding depositors' money.
            vault_capital: strategies
                .iter()
                .filter(|strategy| {
                    matches!(
                        strategy.vault.state,
                        shared::model::vault::VaultState::Open
                            | shared::model::vault::VaultState::Frozen
                    )
                })
                .map(|strategy| strategy.vault.value)
                .sum(),
            trades,
        };

        let mut snapshot = self.snapshot.write().await;
        snapshot.version += 1;
        snapshot.as_of = read::moment(latest);
        snapshot.chains = infos;
        snapshot.strategies = strategies;
        snapshot.authors = authors;
        snapshot.jobs = queue.jobs;
        snapshot.jobs_as_of = queue.as_of;
        snapshot.jobs_stale = queue.stale;
        snapshot.feed = feed;
        snapshot.totals = totals;
    }

    /// Whether every vault chain to be read here is connected and has been
    /// read lately, and what to say: a first line short enough for a
    /// dashboard's row (at most [`HEALTH_LINE`] characters), then a line per
    /// chain.
    pub fn health_at(&self, name: &str, now: Instant) -> (bool, String) {
        let readers = self.readers();
        let mut wrong = Vec::new();
        let mut lines = Vec::new();
        for chain in &self.expected {
            let Some(reader) = readers
                .iter()
                .find(|reader| reader.served.vault_chain == *chain)
            else {
                wrong.push(format!("{chain} not connected"));
                lines.push(format!("{chain}: not connected"));
                continue;
            };
            let read_at = *reader.read_at.lock().unwrap_or_else(|e| e.into_inner());
            let since = read_at.unwrap_or(reader.connected);
            let quiet = now.saturating_duration_since(since);
            if quiet > STALE_AFTER {
                let what = if reader
                    .every_vault_stale
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    format!("every vault stale for {} min", quiet.as_secs() / 60)
                } else if read_at.is_some() {
                    format!("not read for {} min", quiet.as_secs() / 60)
                } else {
                    "never read".to_string()
                };
                wrong.push(format!("{chain} {what}"));
                lines.push(format!("{chain}: {what}"));
            } else if read_at.is_some() {
                lines.push(format!("{chain}: read {} s ago", quiet.as_secs()));
            } else {
                lines.push(format!("{chain}: connected, not read yet"));
            }
        }
        let first = match wrong.as_slice() {
            [] => format!("{name}: {} chains read", self.expected.len()),
            [one] => format!("{name}: {one}"),
            many => format!("{name}: {} chains not read", many.len()),
        };
        let first: String = first.chars().take(HEALTH_LINE).collect();
        let mut said = first;
        for line in lines {
            said.push('\n');
            said.push_str(&line);
        }
        (wrong.is_empty(), said)
    }

    /// The health route's answer: 200 while every chain is read, 503 while
    /// one is not connected or its record has stopped moving, every vault on
    /// it shown as last read whole.
    pub fn health_answer(&self, name: &str) -> (axum::http::StatusCode, String) {
        match self.health_at(name, Instant::now()) {
            (true, said) => (axum::http::StatusCode::OK, said),
            (false, said) => (axum::http::StatusCode::SERVICE_UNAVAILABLE, said),
        }
    }

    async fn read_one(
        &self,
        reader: &Reader,
        health: &HashMap<(Chain, Address), Health>,
    ) -> Result<Read> {
        let served = &reader.served;
        let chain = &served.client;
        let (head, now) = chain.head_and_time().await?;
        let health: HashMap<Address, Health> = health
            .iter()
            .filter(|((on, _), _)| *on == served.vault_chain)
            .map(|((_, vault), report)| (*vault, report.clone()))
            .collect();

        let mut history = reader.history.write().await;
        let strategies = read::strategies(
            chain,
            served.vault_chain,
            &served.deployments,
            &self.store,
            &mut history,
            &health,
            head,
            now,
        )
        .await?;
        // Authors that can't be read leave the authors as last read, and hold
        // up nothing else of the chain's.
        let authors = match read::authors(chain, &served.deployments).await {
            Ok(authors) => authors,
            Err(error) => {
                tracing::warn!(chain = %served.key, %error, "could not read the authors; kept them as last read");
                self.parts
                    .read()
                    .await
                    .get(&served.vault_chain)
                    .map(|read| read.authors.clone())
                    .unwrap_or_default()
            }
        };
        let feed = read::feed(
            chain,
            served.vault_chain,
            &served.deployments,
            &mut history,
            head,
            &strategies,
        )
        .await;
        let trades = strategies
            .iter()
            .map(|strategy| read::trade_count(&history, strategy))
            .sum();
        drop(history);
        Ok(Read {
            info: ChainInfo {
                chain: served.vault_chain,
                chain_id: chain.chain_id,
                block: head,
                as_of: Some(read::moment(now)),
            },
            now,
            strategies,
            trades,
            authors,
            feed,
        })
    }

    /// The builder's queue, with what the chain adds to each job, and the
    /// wallet each job names, by chain and job id.
    async fn jobs(&self) -> Result<(Vec<Job>, HashMap<(Chain, String), Address>)> {
        let Some(builder) = &self.builder else {
            return Ok(Default::default());
        };
        let jobs: Vec<Job> = self
            .http
            .get(format!("{builder}/jobs"))
            .send()
            .await?
            .json()
            .await?;
        let wallets = wallets(&jobs);
        let mut chains: Vec<Chain> = jobs.iter().map(|job| job.chain).collect();
        chains.sort();
        chains.dedup();
        // Each chain's jobs completed at once with every other chain's, so a
        // chain that is slow to answer holds up only its own.
        let completed = futures::future::join_all(chains.iter().map(|chain| {
            let on: Vec<Job> = jobs
                .iter()
                .filter(|job| job.chain == *chain)
                .cloned()
                .collect();
            self.complete_on(*chain, on)
        }))
        .await;
        // A job the build service no longer lists is not kept.
        let listed: HashSet<(Chain, &str)> = jobs
            .iter()
            .map(|job| (job.chain, job.id.as_str()))
            .collect();
        self.completions()
            .retain(|(chain, id), _| listed.contains(&(*chain, id.as_str())));
        let mut completed: HashMap<String, Job> = completed
            .into_iter()
            .flatten()
            .map(|job| (job.id.clone(), job))
            .collect();
        Ok((
            jobs.into_iter()
                .map(|job| completed.remove(&job.id).unwrap_or(job))
                .collect(),
            wallets,
        ))
    }

    /// One job, as the build service gives it, with what its chain adds:
    /// bounded as the queue's are, so a chain slow to answer leaves it with
    /// what was last known of it.
    /// It is called as the snapshot calls its author (see [`shown_names`]).
    pub async fn complete(&self, job: Job) -> Job {
        let chain = job.chain;
        let wallet = job.author.parse::<Address>().ok();
        let fallback = job.clone();
        let mut job = self
            .complete_on(chain, vec![job])
            .await
            .pop()
            .unwrap_or(fallback);
        if let Some(wallet) = wallet {
            let known = self
                .names
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&(chain, wallet))
                .cloned();
            job.author = known.unwrap_or_else(|| first_name(&job.author, wallet));
        }
        job
    }

    /// Jobs on one chain with what the chain adds to each. Each job's reads
    /// are bounded by [`JOB_READ`], and a refresh's by [`JOB_READS`]; every
    /// job completed in time is kept, and the rest are left with what was
    /// last completed of them, to be asked for first next time. Jobs are read
    /// in the order they were last completed, never-completed first, so a
    /// read cut off at the limit keeps its place and every job is read in
    /// turn. A job whose end is settled (activated, refused or failed) and
    /// has been completed is not read again.
    async fn complete_on(&self, chain: Chain, jobs: Vec<Job>) -> Vec<Job> {
        use futures::StreamExt;

        let Some(served) = self.on(chain) else {
            return jobs;
        };
        let mut unsettled: Vec<(Option<Instant>, Job)> = {
            let completions = self.completions();
            jobs.iter()
                .filter_map(|job| {
                    let completion = completions.get(&(job.chain, job.id.clone()));
                    if completion.is_some_and(|was| settled(&was.job)) {
                        return None;
                    }
                    Some((completion.map(|was| was.at), job.clone()))
                })
                .collect()
        };
        // Never completed first, then the longest since it was completed.
        unsettled.sort_by_key(|(at, _)| *at);
        let reads =
            futures::stream::iter(unsettled).for_each_concurrent(JOBS_AT_ONCE, |(_, job)| async {
                let key = (job.chain, job.id.clone());
                let read = read::job(&served.client, &served.deployments, &self.store, job);
                if let Ok(Ok(job)) = tokio::time::timeout(JOB_READ, read).await {
                    self.completions().insert(
                        key,
                        Completion {
                            job,
                            at: Instant::now(),
                        },
                    );
                }
            });
        if tokio::time::timeout(JOB_READS, reads).await.is_err() {
            tracing::debug!(chain = %served.key, "completed part of this chain's jobs, the rest next time");
        }
        let completions = self.completions();
        jobs.into_iter()
            .map(|job| match completions.get(&(job.chain, job.id.clone())) {
                Some(was) => last_known(job, &was.job),
                None => job,
            })
            .collect()
    }

    fn completions(&self) -> std::sync::MutexGuard<'_, HashMap<(Chain, String), Completion>> {
        self.completions
            .lock()
            .expect("the completions lock is never poisoned")
    }
}

/// The wallet each job names, by chain and job id, as the build service gives
/// it (before a chain puts the author's handle in its place).
fn wallets(jobs: &[Job]) -> HashMap<(Chain, String), Address> {
    jobs.iter()
        .filter_map(|job| {
            let wallet = job.author.parse::<Address>().ok()?;
            Some(((job.chain, job.id.clone()), wallet))
        })
        .collect()
}

/// What each author is shown as, by vault chain and address, given what each
/// is called (the handle they set, or their address if they set none) in
/// the order they were read; the first read of each is taken.
///
/// An author is their address on a vault chain, and a handle is only what
/// they are called: `AuthorProfiles` keeps a handle unique by the hash of its
/// exact bytes, so nothing stops a handle that is another's spelt in other
/// case, another's on another chain, or the address another author is shown
/// by. Every name shown is one author's alone, however a page compares names
/// (ignoring case and the spaces around them):
///
/// - a handle naming an address (anything with `0x` and hex digits in it) is
///   shown with the author's own address alongside, as `handle (0x…)`, so it
///   is never taken for another author's address, nor for another's handle
///   shown so;
/// - a handle two authors share is shown so for each of them, never merged;
/// - an author at the same address on two chains, shown the same on each, is
///   shown with the chain's name too.
pub fn shown_names(called: &[((Chain, Address), String)]) -> HashMap<(Chain, Address), String> {
    let mut authors: Vec<((Chain, Address), &str, String)> = Vec::new();
    for ((chain, address), handle) in called {
        if authors.iter().any(|(key, _, _)| *key == (*chain, *address)) {
            continue;
        }
        authors.push((
            (*chain, *address),
            handle.as_str(),
            first_name(handle, *address),
        ));
    }

    let shared = |authors: &[((Chain, Address), &str, String)]| -> Vec<bool> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for (_, _, name) in authors {
            *counts.entry(compared(name)).or_default() += 1;
        }
        authors
            .iter()
            .map(|(_, _, name)| counts[&compared(name)] > 1)
            .collect()
    };
    let twice = shared(&authors);
    for ((key, handle, name), twice) in authors.iter_mut().zip(twice) {
        if twice && set(handle, key.1) {
            *name = format!("{handle} ({})", read::address_of(key.1));
        }
    }
    let twice = shared(&authors);
    for ((key, _, name), twice) in authors.iter_mut().zip(twice) {
        if twice {
            *name = format!("{name} on {}", key.0.label());
        }
    }
    authors
        .into_iter()
        .map(|(key, _, name)| (key, name))
        .collect()
}

/// What an author called `handle` (their address, if they set none) at
/// `address` is shown as before anyone else's name is looked at: the handle,
/// or their address if they set none, with their address alongside a handle
/// that names an address.
fn first_name(handle: &str, address: Address) -> String {
    let own = read::address_of(address);
    if !set(handle, address) {
        own
    } else if names_an_address(handle) {
        format!("{handle} ({own})")
    } else {
        handle.to_string()
    }
}

/// Whether `handle` is one the author at `address` set, rather than their
/// address standing in for one.
fn set(handle: &str, address: Address) -> bool {
    let handle = handle.trim();
    !handle.is_empty() && !handle.eq_ignore_ascii_case(&read::address_of(address))
}

/// Whether `text` has an address in it, or the start of one: `0x` and four
/// hex digits or more, in any case.
fn names_an_address(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.match_indices("0x").any(|(at, _)| {
        lower[at + 2..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .count()
            >= 4
    })
}

/// A name as pages compare names: in any case, the spaces around it aside.
fn compared(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Whether a job's end is settled, so what its chain adds to it no longer
/// changes: it has ended, and an activation's or a refusal's review has been
/// read with it. The build service can say a job was refused before the
/// node the indexer reads shows the review, so a refusal read without its
/// review is read again.
fn settled(job: &Job) -> bool {
    match job.state {
        JobState::Failed { .. } => true,
        JobState::Activated | JobState::Refused => {
            job.review.is_some() || job.strategy_id.is_none()
        }
        _ => false,
    }
}

/// A job as the build service gives it, with what its chain added when it was
/// last completed.
fn last_known(mut job: Job, was: &Job) -> Job {
    job.author = was.author.clone();
    job.review = was.review.clone();
    if was.state == JobState::Activated {
        job.state = JobState::Activated;
        job.strategy_slug = was.strategy_slug.clone();
    }
    job
}

/// How long the reads that complete one job may take.
const JOB_READ: Duration = Duration::from_secs(3);

/// How long one refresh may spend completing one chain's jobs, all of them
/// together. A job not reached in time is reached at a later refresh.
const JOB_READS: Duration = Duration::from_secs(5);

/// How many of one chain's jobs are completed at once.
const JOBS_AT_ONCE: usize = 8;

/// How long between reads of each chain, and of the queue.
const PAUSE: Duration = Duration::from_secs(2);

/// How long a chain may go unread before its health says so: well past a
/// slow read, and far short of a record anyone would trust.
pub const STALE_AFTER: Duration = Duration::from_secs(3 * 60);

/// The longest first line a health route gives: a dashboard shows it in a
/// row.
pub const HEALTH_LINE: usize = 60;

/// How long to wait for another service to take a connection, and to answer.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The one way the API and the indexer ask other services (the build service,
/// the executor): bounded, so one that takes the connection and never answers
/// holds up nothing for good.
pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("an HTTP client with timeouts builds")
}

fn empty_snapshot() -> Snapshot {
    Snapshot {
        version: 0,
        as_of: read::moment(0),
        chains: Vec::new(),
        strategies: Vec::new(),
        authors: Vec::new(),
        jobs: Vec::new(),
        jobs_as_of: None,
        jobs_stale: false,
        feed: Vec::new(),
        totals: Totals {
            vault_capital: 0.0,
            trades: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_dated_more_than_a_few_minutes_ahead_is_refused() {
        let now = time::OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
        let at = |offset: time::Duration| {
            let moment = now + offset;
            time::PrimitiveDateTime::new(moment.date(), moment.time())
        };
        assert!(!from_the_future(at(time::Duration::ZERO), now));
        assert!(!from_the_future(at(-time::Duration::hours(1)), now));
        // A clock a little fast is still believed.
        assert!(!from_the_future(at(time::Duration::minutes(5)), now));
        assert!(from_the_future(at(time::Duration::minutes(6)), now));
        assert!(from_the_future(at(time::Duration::days(365)), now));
    }

    /// Every name shown is one author's alone, compared as a page compares
    /// names: a handle naming an address, one two authors share in any case
    /// or on two chains, is shown with the author's address, and one address
    /// on two chains with the chain too.
    #[test]
    fn every_author_is_shown_by_a_name_of_their_own() {
        let alice: Address = "0xA11cE00000000000000000000000000000000001"
            .parse()
            .unwrap();
        let mallory: Address = "0x3a110700000000000000000000000000000000a2"
            .parse()
            .unwrap();
        let carol = Address::repeat_byte(0xc0);
        let names = shown_names(&[
            ((Chain::Base, alice), read::address_of(alice)),
            (
                (Chain::Base, mallory),
                read::address_of(alice).to_lowercase(),
            ),
            ((Chain::Base, carol), "carol".to_string()),
            ((Chain::Robinhood, alice), "Carol ".to_string()),
            ((Chain::Robinhood, carol), String::new()),
            (
                (Chain::Base, carol),
                "ignored: the first read is taken".to_string(),
            ),
        ]);
        assert_eq!(names.len(), 5);
        assert_eq!(names[&(Chain::Base, alice)], read::address_of(alice));
        assert_eq!(
            names[&(Chain::Base, mallory)],
            format!(
                "{} ({})",
                read::address_of(alice).to_lowercase(),
                read::address_of(mallory)
            )
        );
        assert_eq!(
            names[&(Chain::Base, carol)],
            format!("carol ({})", read::address_of(carol))
        );
        assert_eq!(
            names[&(Chain::Robinhood, alice)],
            format!("Carol  ({})", read::address_of(alice))
        );
        assert_eq!(names[&(Chain::Robinhood, carol)], read::address_of(carol));
        let mut compared: Vec<String> = names.values().map(|name| compared(name)).collect();
        compared.sort();
        compared.dedup();
        assert_eq!(compared.len(), names.len(), "{names:?}");

        // The same address, called the same, on two chains.
        let names = shown_names(&[
            ((Chain::Base, carol), "carol".to_string()),
            ((Chain::Robinhood, carol), "carol".to_string()),
        ]);
        assert_eq!(
            names[&(Chain::Base, carol)],
            format!(
                "carol ({}) on {}",
                read::address_of(carol),
                Chain::Base.label()
            )
        );
        assert_ne!(
            names[&(Chain::Base, carol)],
            names[&(Chain::Robinhood, carol)]
        );

        // A handle of one's own, in no one else's way, is shown as it is.
        let names = shown_names(&[((Chain::Base, carol), "carol".to_string())]);
        assert_eq!(names[&(Chain::Base, carol)], "carol");
    }

    #[test]
    fn a_report_from_the_future_is_refused_as_unprocessable() {
        assert_eq!(
            HealthVerdict::FromTheFuture.status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
