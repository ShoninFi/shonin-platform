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
pub mod solana;
pub mod store;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use anyhow::Result;
use chain::bindings::attestation::{IAttestationRegistry, ImageKind};
use chain::bindings::vault::IVault;
use chain::{ChainKey, Served, SolanaServed};
use shared::model::health::Health;
use shared::model::registry::{Author, FeedEvent, Job, JobState};
use shared::{Chain, ChainAddress};
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

/// Who signed a report on the Solana cluster `cluster` stands for (the value
/// the vault program binds its digests to there): the key the post names, if
/// the signature over the report's digest for that cluster is that key's.
/// ed25519 has no recovery, so the post carries its key and the signature is
/// checked against it. None when the signature or the key is malformed, or
/// the signature is not the key's.
fn ed25519_signer(cluster: &[u8; 32], post: &HealthPost) -> Option<Option<[u8; 32]>> {
    let key = post.signer.as_deref()?;
    let key: [u8; 32] = bs58::decode(key).into_vec().ok()?.try_into().ok()?;
    let signature = match post.signature.strip_prefix("0x") {
        Some(hex) => alloy::hex::decode(hex).ok()?,
        None => bs58::decode(&post.signature).into_vec().ok()?,
    };
    let signature = ed25519_dalek::Signature::from_slice(&signature).ok()?;
    let verifying = ed25519_dalek::VerifyingKey::from_bytes(&key).ok()?;
    let digest = HealthPost::solana_digest(cluster, &post.vault, &post.report);
    Some(
        verifying
            .verify_strict(&digest, &signature)
            .is_ok()
            .then_some(key),
    )
}

/// Whether `signer` is blessed for `vault` and the bundle it runs, and as an
/// executor, at the cluster's clock: the registry's blessing for the vault,
/// the key's record, and its image's and project's, read and judged as the
/// programs judge them.
async fn solana_blessed_executor(
    served: &SolanaServed,
    vault: &chain::solana::Pubkey,
    signer: &chain::solana::Pubkey,
) -> Result<bool> {
    use chain::solana::programs::registry::{read as at, state};

    let client = &served.client;
    let Some(account) = client
        .load::<chain::solana::programs::vault::state::Vault>(vault)
        .await?
    else {
        return Ok(false);
    };
    let now = client.now().await?;
    let config: state::Config = served.platform.registry_config(client).await?;
    let Some(blessing) = client
        .load::<state::Blessing>(&at::blessing_address(
            signer,
            &served.platform.cluster,
            &vault.to_bytes(),
            &account.bundle_hash,
        ))
        .await?
    else {
        return Ok(false);
    };
    let Some(key) = client
        .load::<state::KeyInfo>(&at::key_address(signer))
        .await?
    else {
        return Ok(false);
    };
    let image = client
        .load::<state::ImageRecord>(&at::image_address(&key.image_digest))
        .await?;
    let project = client
        .load::<state::ProjectRecord>(&at::project_address(&key.project_id))
        .await?;
    Ok(blessing.signer == *signer
        && state::is_blessed_for(&config, &blessing, image.as_ref(), project.as_ref(), now)
        && state::is_blessed_as(
            &config,
            &key,
            image.as_ref(),
            project.as_ref(),
            state::ImageKind::Executor,
            now,
        ))
}

/// One vault chain, as the indexer reads it.
pub struct Reader {
    pub vault_chain: Chain,
    pub key: ChainKey,
    source: Source,
    /// When it connected, and when it was last read whole: what its health
    /// is judged by.
    connected: Instant,
    read_at: std::sync::Mutex<Option<Instant>>,
    /// Whether the last read of it found every vault on it stale: kept as
    /// last read whole, none read whole now. Such a read is not a read of
    /// the chain, and moves `read_at` on no further.
    every_vault_stale: std::sync::atomic::AtomicBool,
}

/// What a chain is read through: an EVM client and what has been made of its
/// logs, or a Solana client and what has been made of each vault's history.
enum Source {
    Evm {
        served: Box<Served>,
        history: Box<RwLock<History>>,
    },
    Solana {
        served: Box<SolanaServed>,
        history: Box<RwLock<solana::History>>,
    },
}

impl Reader {
    /// The EVM chain this reads, if it is one.
    pub fn evm(&self) -> Option<&Served> {
        match &self.source {
            Source::Evm { served, .. } => Some(served),
            Source::Solana { .. } => None,
        }
    }

    /// The Solana chain this reads, if it is one.
    pub fn solana(&self) -> Option<&SolanaServed> {
        match &self.source {
            Source::Solana { served, .. } => Some(served),
            Source::Evm { .. } => None,
        }
    }

    /// The chain as a snapshot names it before it is first read.
    fn unread_info(&self) -> ChainInfo {
        ChainInfo {
            chain: self.vault_chain,
            key: self.key.to_string(),
            chain_id: self.evm().map(|served| served.client.chain_id),
            block: 0,
            as_of: None,
        }
    }
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
    authors: Vec<(ChainAddress, Author)>,
    feed: Vec<FeedEvent>,
}

/// The build queue as last read: the jobs the build service last gave, when
/// it gave them, and whether it has failed to answer since.
#[derive(Clone, Default)]
struct Queue {
    jobs: Vec<Job>,
    /// The wallet each job names, by chain and job id, as the build service
    /// gave it: who the job's author is, whatever they are called.
    wallets: HashMap<(Chain, String), ChainAddress>,
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
    /// A Solana chain, connected and read.
    Solana(Box<SolanaServed>),
    /// To be read here, and not connected yet.
    NotConnected,
    /// Not read here at all.
    NotServed,
}

/// Every vault chain a service's arguments name, EVM and Solana, and each
/// one as it connects: one out of reach holds up none of the others.
pub struct Connections {
    pub expected: Vec<Chain>,
    pub service_rpc_policy: chain::rpc_policy::ServiceRpcPolicy,
    pub evm: tokio::sync::mpsc::UnboundedReceiver<Served>,
    pub solana: tokio::sync::mpsc::UnboundedReceiver<SolanaServed>,
}

/// Start connecting to every vault chain `args` name. A service told to
/// serve one Solana chain serves no EVM chain.
pub fn connections(args: &chain::ChainArgs) -> Result<Connections> {
    let solana = args.solana_targets()?;
    let only_solana = args.vault_chain.is_some() && !solana.is_empty();
    let (evm_chains, evm) = if only_solana {
        let (_, none) = tokio::sync::mpsc::unbounded_channel();
        (Vec::new(), none)
    } else {
        let chains: Vec<Chain> = args
            .targets()?
            .iter()
            .map(|target| target.vault_chain)
            .collect();
        (chains, args.connect_each()?)
    };
    let mut expected = evm_chains;
    expected.extend(solana.iter().map(|target| target.vault_chain));
    Ok(Connections {
        service_rpc_policy: chain::config::ChainConfigs::load(&args.chains)?
            .service_rpc_policy()
            .clone(),
        expected,
        evm,
        solana: args.connect_each_solana()?,
    })
}

/// The record, as read from every vault chain, and what it needs to keep
/// reading.
pub struct Indexer {
    service_rpc_policy: chain::rpc_policy::ServiceRpcPolicy,
    /// The vault chains to be read, connected or not yet: a chain is read
    /// from the moment it connects, and one that hasn't holds up no other.
    expected: Vec<Chain>,
    readers: std::sync::RwLock<Vec<Arc<Reader>>>,
    pub store: Store,
    /// Where the build service answers, for the queue. Without one the record
    /// is the chains alone and the queue reads as empty.
    pub builder: Option<String>,
    /// Where the build service's last answer for its queue is kept, so the
    /// queue is still shown after a restart while the build service is off
    /// ([`Indexer::with_jobs_cache`]).
    jobs_cache: Option<PathBuf>,
    /// The queue as the cache had it at start, until it is shown or the
    /// build service answers, and when it was written.
    cached_jobs: std::sync::Mutex<Option<(Vec<Job>, Option<time::PrimitiveDateTime>)>>,
    /// What was last written to the cache, so an unchanged queue is not
    /// written again.
    jobs_written: std::sync::Mutex<Vec<u8>>,
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
    pub health: RwLock<HashMap<(Chain, ChainAddress), Health>>,
    /// The same reports as they were posted, signature and all, so anyone
    /// (a successor executor above all) can check for themselves who signed
    /// the latest one and when.
    signed: RwLock<HashMap<(Chain, ChainAddress), HealthPost>>,
    /// What each author is shown as, by vault chain and address, as the last
    /// snapshot had it (see [`shown_names`]).
    names: std::sync::RwLock<HashMap<(Chain, ChainAddress), String>>,
}

impl Indexer {
    /// An indexer for `expected`, reading none of them until each is
    /// handed over by [`Indexer::add`].
    pub fn new(expected: Vec<Chain>, store: PathBuf, builder: Option<String>) -> Self {
        Self {
            service_rpc_policy: chain::rpc_policy::ServiceRpcPolicy::default(),
            snapshot: RwLock::new(empty_snapshot()),
            expected,
            readers: std::sync::RwLock::new(Vec::new()),
            health: RwLock::new(HashMap::new()),
            signed: RwLock::new(HashMap::new()),
            store: Store::new(store),
            builder,
            jobs_cache: None,
            cached_jobs: std::sync::Mutex::new(None),
            jobs_written: std::sync::Mutex::new(Vec::new()),
            http: http(),
            parts: RwLock::new(HashMap::new()),
            queue: RwLock::new(Queue::default()),
            completions: std::sync::Mutex::new(HashMap::new()),
            names: std::sync::RwLock::new(HashMap::new()),
        }
    }

    /// Use validated process-wide queue and connection-discovery polls.
    pub fn with_service_rpc_policy(
        mut self,
        policy: chain::rpc_policy::ServiceRpcPolicy,
    ) -> Result<Self> {
        policy.validate()?;
        self.service_rpc_policy = policy;
        Ok(self)
    }

    /// The same indexer, its store reading through to the public copy at
    /// `base` for any file it doesn't hold (see [`Store::with_remote`]).
    pub fn with_store_url(mut self, base: &str) -> Self {
        self.store = Store::with_remote(self.store.root().to_path_buf(), base);
        self
    }

    /// The same indexer, keeping the build service's last answer for its
    /// queue at `path` and showing it, marked stale, while the build service
    /// is off: after a restart as before one. A cache that can't be read is
    /// no queue.
    pub fn with_jobs_cache(mut self, path: PathBuf) -> Self {
        let cached = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<Job>>(&bytes).ok())
            .map(|jobs| {
                let at = std::fs::metadata(&path)
                    .and_then(|meta| meta.modified())
                    .ok()
                    .map(|modified| {
                        let at = time::OffsetDateTime::from(modified);
                        time::PrimitiveDateTime::new(at.date(), at.time())
                    });
                (jobs, at)
            });
        *self.cached_jobs.lock().unwrap_or_else(|e| e.into_inner()) = cached;
        self.jobs_cache = Some(path);
        self
    }

    /// Keep the build service's answer for its queue in the cache, if there
    /// is one and the answer changed. Written aside and moved into place, so
    /// a restart never reads half of it.
    fn keep_jobs(&self, jobs: &[Job]) {
        let Some(path) = &self.jobs_cache else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(jobs) else {
            return;
        };
        let mut written = self.jobs_written.lock().unwrap_or_else(|e| e.into_inner());
        if *written == bytes {
            return;
        }
        let aside = path.with_extension("json.part");
        let kept = path
            .parent()
            .is_none_or(|directory| std::fs::create_dir_all(directory).is_ok())
            && std::fs::write(&aside, &bytes).is_ok()
            && std::fs::rename(&aside, path).is_ok();
        if kept {
            *written = bytes;
        } else {
            tracing::warn!(path = %path.display(), "the build queue could not be kept on disk");
        }
    }

    /// Start reading a chain that has connected.
    pub async fn add(&self, served: Served) {
        let (vault_chain, key) = (served.vault_chain, served.key.clone());
        self.add_reader(
            vault_chain,
            key,
            Source::Evm {
                served: Box::new(served),
                history: Box::new(RwLock::new(History::default())),
            },
        )
        .await;
    }

    /// Start reading a Solana chain that has connected.
    pub async fn add_solana(&self, served: SolanaServed) {
        let (vault_chain, key) = (served.vault_chain, served.key.clone());
        self.add_reader(
            vault_chain,
            key,
            Source::Solana {
                served: Box::new(served),
                history: Box::new(RwLock::new(solana::History::default())),
            },
        )
        .await;
    }

    async fn add_reader(&self, vault_chain: Chain, key: ChainKey, source: Source) {
        tracing::info!(chain = %key, "the indexer is reading");
        let reader = Reader {
            vault_chain,
            key,
            source,
            connected: Instant::now(),
            read_at: std::sync::Mutex::new(None),
            every_vault_stale: std::sync::atomic::AtomicBool::new(false),
        };
        let info = reader.unread_info();
        {
            let mut readers = self.readers.write().unwrap_or_else(|e| e.into_inner());
            if readers.iter().any(|known| known.vault_chain == vault_chain) {
                return;
            }
            readers.push(Arc::new(reader));
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

    /// Read every chain of `connections` from the moment each connects.
    pub fn take(self: &Arc<Self>, connections: Connections) {
        let evm = self.clone();
        tokio::spawn(async move { evm.add_each(connections.evm).await });
        let solana = self.clone();
        tokio::spawn(async move { solana.add_each_solana(connections.solana).await });
    }

    /// Read each Solana chain from the moment it is handed over.
    pub async fn add_each_solana(
        &self,
        mut connected: tokio::sync::mpsc::UnboundedReceiver<SolanaServed>,
    ) {
        while let Some(served) = connected.recv().await {
            self.add_solana(served).await;
        }
    }

    /// Every chain read so far.
    pub fn readers(&self) -> Vec<Arc<Reader>> {
        self.readers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The chain a vault chain is read from, once it has connected, if it
    /// is an EVM one.
    pub fn on(&self, chain: Chain) -> Option<Served> {
        self.reader(chain).and_then(|reader| reader.evm().cloned())
    }

    fn reader(&self, chain: Chain) -> Option<Arc<Reader>> {
        self.readers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|reader| reader.vault_chain == chain)
            .cloned()
    }

    /// Whether a vault chain is read here, not connected yet, or not read
    /// here at all.
    pub fn lookup(&self, chain: Chain) -> Lookup {
        match self.reader(chain) {
            Some(reader) => match &reader.source {
                Source::Evm { served, .. } => Lookup::Served(served.clone()),
                Source::Solana { served, .. } => Lookup::Solana(served.clone()),
            },
            None if self.expected.contains(&chain) => Lookup::NotConnected,
            None => Lookup::NotServed,
        }
    }

    /// What `owner` paid for what they hold in a Solana vault, as the vault's
    /// history has been read so far. None until it has been read.
    pub async fn solana_cost(
        &self,
        chain: Chain,
        vault: &chain::solana::Pubkey,
        owner: &chain::solana::Pubkey,
    ) -> Option<f64> {
        let reader = self.reader(chain)?;
        let Source::Solana { history, .. } = &reader.source else {
            return None;
        };
        solana::cost(&*history.read().await, vault, owner)
    }

    /// Take a health report if the key that signed it is blessed for the
    /// vault on this chain, and it is newer than the one already held. Only
    /// the instance running a strategy can write about it, and nobody can
    /// put an older report back.
    pub async fn take_health(&self, chain: Chain, post: HealthPost) -> HealthVerdict {
        // A post that names its key is an ed25519 one, as a Solana executor
        // signs; one that doesn't is recovered, as an EVM executor's is.
        // Either is checked whole before the chain is asked anything. An
        // ed25519 one is checked against the cluster this service connected
        // to, which its digest binds.
        if post.signer.is_some() {
            if post.vault.parse::<chain::solana::Pubkey>().is_err() {
                return HealthVerdict::Malformed;
            }
            let served = match self.lookup(chain) {
                Lookup::Solana(served) => served,
                // An ed25519 post about an EVM chain's vault: no key it could
                // name is blessed there.
                Lookup::Served(_) => return HealthVerdict::Malformed,
                Lookup::NotConnected => return HealthVerdict::NotConnected,
                Lookup::NotServed => return HealthVerdict::UnknownChain,
            };
            let Some(signer) = ed25519_signer(&served.client.cluster_value(), &post) else {
                return HealthVerdict::Malformed;
            };
            if from_the_future(post.report.updated, time::OffsetDateTime::now_utc()) {
                return HealthVerdict::FromTheFuture;
            }
            return self.take_solana_health(chain, *served, post, signer).await;
        }
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
            // A Solana vault's report names its key; one that doesn't can't
            // be checked.
            Lookup::Solana(_) => return HealthVerdict::Malformed,
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
        self.keep_health(chain, ChainAddress::Evm(vault.0.0), post)
            .await
    }

    /// A report about a Solana vault: signed with ed25519 by the key the post
    /// names, and taken only if that key is blessed for the vault and its
    /// bundle as the vault's executor, at the cluster's own clock.
    async fn take_solana_health(
        &self,
        chain: Chain,
        served: SolanaServed,
        post: HealthPost,
        signer: Option<[u8; 32]>,
    ) -> HealthVerdict {
        use chain::solana::Pubkey;

        let Ok(vault) = post.vault.parse::<Pubkey>() else {
            return HealthVerdict::Malformed;
        };
        let Some(signer) = signer.map(Pubkey::new_from_array) else {
            tracing::warn!(%vault, "a health report whose signature is not its key's");
            return HealthVerdict::NotBlessed;
        };
        match solana_blessed_executor(&served, &vault, &signer).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(%vault, %signer, "a health report from a key this vault has not blessed as its executor");
                return HealthVerdict::NotBlessed;
            }
            Err(error) => {
                tracing::warn!(%vault, error = format!("{error:#}"), "could not read the cluster for a health report");
                return HealthVerdict::Unavailable;
            }
        }
        self.keep_health(chain, ChainAddress::Solana(vault.to_bytes()), post)
            .await
    }

    /// Keep a report checked as its vault's executor's, unless one at least
    /// as new is held.
    async fn keep_health(
        &self,
        chain: Chain,
        vault: ChainAddress,
        post: HealthPost,
    ) -> HealthVerdict {
        let mut health = self.health.write().await;
        if health
            .get(&(chain, vault))
            .is_some_and(|held| held.updated >= post.report.updated)
        {
            return HealthVerdict::Stale;
        }
        // Both under the report lock, so the two never disagree on which
        // report is the latest.
        self.signed
            .write()
            .await
            .insert((chain, vault), post.clone());
        health.insert((chain, vault), post.report);
        HealthVerdict::Taken
    }

    /// The latest report taken for a vault, as its executor posted it.
    pub async fn signed_health(&self, chain: Chain, vault: ChainAddress) -> Option<HealthPost> {
        self.signed.read().await.get(&(chain, vault)).cloned()
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
                tokio::time::sleep(Duration::from_millis(
                    self.service_rpc_policy.indexer_queue_poll_ms,
                ))
                .await;
            }
        }));
        let mut started = std::collections::HashSet::new();
        loop {
            for reader in self.readers() {
                if started.insert(reader.vault_chain) {
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
                _ = tokio::time::sleep(Duration::from_millis(self.service_rpc_policy.indexer_discovery_poll_ms)) => {}
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
    /// that found every vault on it stale (a vault never read whole among
    /// them) is put in place too, each vault marked, but is not taken for a
    /// read of the chain: its health says so once that has lasted (see
    /// [`Indexer::health_at`]).
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
                self.parts.write().await.insert(reader.vault_chain, read);
                if !every_vault_stale {
                    *reader.read_at.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(Instant::now());
                }
                self.compose().await;
            }
            Err(error) => {
                tracing::warn!(chain = %reader.key, error = format!("{error:#}"), "could not read this chain");
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
                // The queue kept from before a restart, shown as it was last
                // given, until the build service answers again.
                let cached = self
                    .cached_jobs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                match cached {
                    Some((jobs, as_of)) => {
                        let (jobs, wallets) = self.completed(jobs).await;
                        *self.queue.write().await = Queue {
                            jobs,
                            wallets,
                            as_of,
                            stale: true,
                        };
                    }
                    None => self.queue.write().await.stale = true,
                }
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
        let mut authors: Vec<((Chain, ChainAddress), Author)> = Vec::new();
        let mut feed = Vec::new();
        let mut trades = 0;
        for reader in &readers {
            let Some(read) = parts.get(&reader.vault_chain) else {
                // Connected, and not read yet.
                infos.push(reader.unread_info());
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
                let key = (reader.vault_chain, *address);
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
        let mut handles: Vec<((Chain, ChainAddress), String)> = authors
            .iter()
            .map(|(key, author)| (*key, author.handle.clone()))
            .collect();
        for strategy in &strategies {
            if let Ok(address) = strategy.author_address.parse::<ChainAddress>() {
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
                .parse::<ChainAddress>()
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
            // Every vault still holding depositors' money. A vault never read
            // whole has no figures to add: it is in the record, marked so,
            // and in no total.
            vault_capital: strategies
                .iter()
                .filter(|strategy| !read::never_read(strategy))
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
            let Some(reader) = readers.iter().find(|reader| reader.vault_chain == *chain) else {
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
        health: &HashMap<(Chain, ChainAddress), Health>,
    ) -> Result<Read> {
        let (served, history) = match &reader.source {
            Source::Evm { served, history } => (served, history),
            Source::Solana { served, history } => {
                return self.read_solana(reader, served, history, health).await;
            }
        };
        let chain = &served.client;
        let (head, now) = chain.head_and_time().await?;
        let health: HashMap<Address, Health> = health
            .iter()
            .filter(|((on, _), _)| *on == served.vault_chain)
            .filter_map(|((_, vault), report)| Some((Address::from(vault.evm()?), report.clone())))
            .collect();

        let mut history = history.write().await;
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
            Ok(authors) => authors
                .into_iter()
                .map(|(address, author)| (ChainAddress::Evm(address.0.0), author))
                .collect(),
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
                key: reader.key.to_string(),
                chain_id: Some(chain.chain_id),
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

    /// One Solana chain's part: its vaults as they are now, each one's
    /// record from its own history.
    async fn read_solana(
        &self,
        reader: &Reader,
        served: &SolanaServed,
        history: &RwLock<solana::History>,
        health: &HashMap<(Chain, ChainAddress), Health>,
    ) -> Result<Read> {
        let client = &served.client;
        let slot = client.rpc.slot().await?;
        let now = client.now().await?;
        let health: HashMap<chain::solana::Pubkey, Health> = health
            .iter()
            .filter(|((on, _), _)| *on == served.vault_chain)
            .filter_map(|((_, vault), report)| {
                Some((
                    chain::solana::Pubkey::new_from_array(vault.solana()?),
                    report.clone(),
                ))
            })
            .collect();
        let mut history = history.write().await;
        let read = solana::read(served, &self.store, &mut history, &health, now).await?;
        drop(history);
        let feed = solana::feed(served.vault_chain, &read.strategies);
        Ok(Read {
            info: ChainInfo {
                chain: served.vault_chain,
                key: reader.key.to_string(),
                chain_id: None,
                block: slot,
                as_of: Some(read::moment(now)),
            },
            now,
            strategies: read.strategies,
            trades: 0,
            authors: read.authors,
            feed,
        })
    }

    /// The builder's queue, with what the chain adds to each job, and the
    /// wallet each job names, by chain and job id.
    async fn jobs(&self) -> Result<(Vec<Job>, HashMap<(Chain, String), ChainAddress>)> {
        let Some(builder) = &self.builder else {
            return Ok(Default::default());
        };
        let jobs: Vec<Job> = self
            .http
            .get(format!("{builder}/jobs"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        self.keep_jobs(&jobs);
        // The build service has answered: what the cache held is out of date.
        self.cached_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        Ok(self.completed(jobs).await)
    }

    /// The build service's jobs with what each chain adds to them, and the
    /// wallet each names, by chain and job id.
    async fn completed(
        &self,
        jobs: Vec<Job>,
    ) -> (Vec<Job>, HashMap<(Chain, String), ChainAddress>) {
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
        (
            jobs.into_iter()
                .map(|job| completed.remove(&job.id).unwrap_or(job))
                .collect(),
            wallets,
        )
    }

    /// One job, as the build service gives it, with what its chain adds:
    /// bounded as the queue's are, so a chain slow to answer leaves it with
    /// what was last known of it.
    /// It is called as the snapshot calls its author (see [`shown_names`]).
    pub async fn complete(&self, job: Job) -> Job {
        let chain = job.chain;
        let wallet = job.author.parse::<ChainAddress>().ok();
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

        let Some(reader) = self
            .readers()
            .into_iter()
            .find(|reader| reader.vault_chain == chain)
        else {
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
                let read = async {
                    match &reader.source {
                        Source::Evm { served, .. } => {
                            read::job(&served.client, &served.deployments, &self.store, job).await
                        }
                        Source::Solana { served, .. } => {
                            solana::job(served, &self.store, job).await
                        }
                    }
                };
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
            tracing::debug!(chain = %reader.key, "completed part of this chain's jobs, the rest next time");
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
fn wallets(jobs: &[Job]) -> HashMap<(Chain, String), ChainAddress> {
    jobs.iter()
        .filter_map(|job| {
            let wallet = job.author.parse::<ChainAddress>().ok()?;
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
pub fn shown_names(
    called: &[((Chain, ChainAddress), String)],
) -> HashMap<(Chain, ChainAddress), String> {
    let mut authors: Vec<((Chain, ChainAddress), &str, String)> = Vec::new();
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

    let shared = |authors: &[((Chain, ChainAddress), &str, String)]| -> Vec<bool> {
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
            *name = format!("{handle} ({})", shown_address(&key.1));
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
fn first_name(handle: &str, address: ChainAddress) -> String {
    let own = shown_address(&address);
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
fn set(handle: &str, address: ChainAddress) -> bool {
    let handle = handle.trim();
    !handle.is_empty()
        && match address {
            ChainAddress::Evm(_) => !handle.eq_ignore_ascii_case(&shown_address(&address)),
            ChainAddress::Solana(_) => handle != shown_address(&address),
        }
}

/// An address as the record shows it: an EVM one checksummed, a Solana key
/// in base58.
pub fn shown_address(address: &ChainAddress) -> String {
    match address {
        ChainAddress::Evm(bytes) => read::address_of(Address::from(*bytes)),
        ChainAddress::Solana(_) => address.to_string(),
    }
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

/// How long between reads of each chain.
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

    fn a_post(updated: u64, signature: &str) -> HealthPost {
        HealthPost {
            vault: "0x1111111111111111111111111111111111111111".to_string(),
            report: Health {
                signals: vec![],
                entries: vec![],
                updated: read::moment(updated),
                signed_by: "0x22".to_string(),
                at: updated,
            },
            signature: signature.to_string(),
            signer: None,
        }
    }

    /// The latest report taken is kept as it was posted, signature and all,
    /// for anyone to check; an older one posted later replaces neither it
    /// nor the report the record shows.
    #[tokio::test]
    async fn the_latest_report_taken_is_kept_as_posted_and_an_older_one_replaces_nothing() {
        let indexer = Indexer::new(vec![Chain::Base], std::env::temp_dir(), None);
        let vault = ChainAddress::Evm([0x11; 20]);
        assert!(indexer.signed_health(Chain::Base, vault).await.is_none());

        let taken = indexer
            .keep_health(Chain::Base, vault, a_post(1_800_000_100, "0xnewer"))
            .await;
        assert!(matches!(taken, HealthVerdict::Taken));
        let stale = indexer
            .keep_health(Chain::Base, vault, a_post(1_800_000_000, "0xolder"))
            .await;
        assert!(matches!(stale, HealthVerdict::Stale));

        let held = indexer.signed_health(Chain::Base, vault).await.unwrap();
        assert_eq!(held.signature, "0xnewer");
        assert_eq!(held.report.at, 1_800_000_100);
        assert_eq!(
            indexer.health.read().await[&(Chain::Base, vault)].at,
            1_800_000_100
        );
        // Another chain's vault at the same address is another vault.
        assert!(
            indexer
                .signed_health(Chain::Hyperliquid, vault)
                .await
                .is_none()
        );
    }

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
        let evm = |address: Address| ChainAddress::Evm(address.0.0);
        let alice = evm("0xA11cE00000000000000000000000000000000001"
            .parse()
            .unwrap());
        let mallory = evm("0x3a110700000000000000000000000000000000a2"
            .parse()
            .unwrap());
        let carol = evm(Address::repeat_byte(0xc0));
        let names = shown_names(&[
            ((Chain::Base, alice), shown_address(&alice)),
            ((Chain::Base, mallory), shown_address(&alice).to_lowercase()),
            ((Chain::Base, carol), "carol".to_string()),
            ((Chain::Robinhood, alice), "Carol ".to_string()),
            ((Chain::Robinhood, carol), String::new()),
            (
                (Chain::Base, carol),
                "ignored: the first read is taken".to_string(),
            ),
        ]);
        assert_eq!(names.len(), 5);
        assert_eq!(names[&(Chain::Base, alice)], shown_address(&alice));
        assert_eq!(
            names[&(Chain::Base, mallory)],
            format!(
                "{} ({})",
                shown_address(&alice).to_lowercase(),
                shown_address(&mallory)
            )
        );
        assert_eq!(
            names[&(Chain::Base, carol)],
            format!("carol ({})", shown_address(&carol))
        );
        assert_eq!(
            names[&(Chain::Robinhood, alice)],
            format!("Carol  ({})", shown_address(&alice))
        );
        assert_eq!(names[&(Chain::Robinhood, carol)], shown_address(&carol));
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
                shown_address(&carol),
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

    /// A Solana author is shown by their key as written: base58 is
    /// case-sensitive, so a handle that is their key in another case is a
    /// handle of its own, and their key stands for no handle.
    #[test]
    fn a_solana_author_with_no_handle_is_shown_by_their_key() {
        let key = ChainAddress::Solana([9; 32]);
        let names = shown_names(&[((Chain::Solana, key), key.to_string())]);
        assert_eq!(names[&(Chain::Solana, key)], key.to_string());
        let names = shown_names(&[((Chain::Solana, key), "dana".to_string())]);
        assert_eq!(names[&(Chain::Solana, key)], "dana");
    }

    /// A Solana report names its key, and is that key's only if the
    /// signature over its digest verifies against it: a report signed by one
    /// key and naming another, or signed for another vault, is not taken.
    #[test]
    fn a_solana_report_is_its_named_key_s_only_if_the_signature_verifies() {
        use ed25519_dalek::Signer as _;

        let signing = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let other = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let key = signing.verifying_key().to_bytes();
        let vault = bs58::encode([8u8; 32]).into_string();
        let report = Health {
            signals: vec![],
            entries: vec![],
            updated: read::moment(1_800_000_000),
            signed_by: bs58::encode(key).into_string(),
            at: 0,
        };
        let localnet = chain::solana::programs::common::digest::cluster_value("localnet");
        let post = |by: &ed25519_dalek::SigningKey, about: &str, named: [u8; 32]| HealthPost {
            vault: vault.clone(),
            signature: bs58::encode(
                by.sign(&HealthPost::solana_digest(&localnet, about, &report))
                    .to_bytes(),
            )
            .into_string(),
            report: report.clone(),
            signer: Some(bs58::encode(named).into_string()),
        };
        assert_eq!(
            ed25519_signer(&localnet, &post(&signing, &vault, key)),
            Some(Some(key))
        );
        assert_eq!(
            ed25519_signer(&localnet, &post(&other, &vault, key)),
            Some(None)
        );
        let elsewhere = bs58::encode([9u8; 32]).into_string();
        assert_eq!(
            ed25519_signer(&localnet, &post(&signing, &elsewhere, key)),
            Some(None)
        );
        let mut garbled = post(&signing, &vault, key);
        garbled.signature = "not a signature".to_string();
        assert_eq!(ed25519_signer(&localnet, &garbled), None);
    }

    /// A vault has the same address on every cluster: a report signed for
    /// it on one cluster is not taken on another.
    #[test]
    fn a_solana_report_signed_for_one_cluster_is_refused_on_another() {
        use ed25519_dalek::Signer as _;

        let signing = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let key = signing.verifying_key().to_bytes();
        let vault = bs58::encode([8u8; 32]).into_string();
        let report = Health {
            signals: vec![],
            entries: vec![],
            updated: read::moment(1_800_000_000),
            signed_by: bs58::encode(key).into_string(),
            at: 0,
        };
        let devnet = chain::solana::programs::common::digest::cluster_value("devnet");
        let mainnet = chain::solana::programs::common::digest::cluster_value("mainnet-beta");
        let post = HealthPost {
            vault: vault.clone(),
            signature: bs58::encode(
                signing
                    .sign(&HealthPost::solana_digest(&devnet, &vault, &report))
                    .to_bytes(),
            )
            .into_string(),
            report: report.clone(),
            signer: Some(bs58::encode(key).into_string()),
        };
        assert_eq!(ed25519_signer(&devnet, &post), Some(Some(key)));
        assert_eq!(ed25519_signer(&mainnet, &post), Some(None));
    }

    #[test]
    fn a_report_from_the_future_is_refused_as_unprocessable() {
        assert_eq!(
            HealthVerdict::FromTheFuture.status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
