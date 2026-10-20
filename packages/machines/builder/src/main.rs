//! The build service.
//!
//! An author uploads a project, then pays to publish it from the same wallet.
//! The payment admits the job and gives the agent its id. The reviewer reads
//! the source before anything is built; if it lets the source through, the
//! service compiles it with a pinned recipe, loads the component, and
//! registers what it made, with the reviewer's go-ahead and its own signature.
//!
//! The queue is public and first come, first served, one job in flight per
//! author. A job ends ready, refused or failed, and shows nothing more than
//! that: no build log is ever kept.
//!
//! An upload is anyone's, naming any wallet, until it is paid for, so what
//! unpaid uploads may hold is bounded: a wallet has at most three waiting,
//! and the service at most fifty. Nobody proves they own the wallet an upload
//! names, so a bound never turns an upload away: past one, the oldest upload
//! waiting under it is forgotten to make room. One is also forgotten a day
//! after it arrived. Unpaid uploads are not listed publicly. A job's archive
//! is kept, in a file of its own, only until the job ends.
//!
//! Forgetting an upload must never cost anyone a payment: the service halts
//! rather than steal. An upload forgotten for its age is forgotten only once
//! the payments read reach a day past it in the chain's own time, so a
//! payment the reader has not got to yet is never outrun. What is forgotten
//! leaves a note behind for a week, and a payment that names it then is kept
//! as a credit: the same upload, sent again by the same author, is admitted
//! as paid. So is a second payment for an upload already paid for.
//!
//! Payments are read only once they are a few blocks deep, and a few blocks
//! behind where the last look ended are read again each time, so a payment in
//! a block a reorg replaced, or one a node behind the head had not got yet,
//! is still read. Where payments were read to moves on only once what they
//! admitted is written down.
//!
//! A chain far behind is caught up a few pages a turn. Blocks the registry's
//! record proves hold no payment are passed over unread: its count at past
//! blocks says where the next payment is, and the times its submissions were
//! paid at confirm it. A page whose logs keep failing is passed over only
//! once that record gives every payment in it, which is then admitted from
//! the record; otherwise that chain's payments wait, said at warn, and the
//! chain is left out of the wake plan meanwhile, so it neither keeps the
//! build service up nor wakes it for nothing.

mod build;
// The same as the reviewer's: what a machine that runs on demand keeps off
// itself, and how it asks the platform's API for it.
// What only the reviewer uses of it goes unused here.
#[path = "../../reviewer/src/cloud.rs"]
#[allow(dead_code)]
mod cloud;
mod isolation;
// The same as the reviewer's: a service's key on a Solana cluster.
#[path = "../../reviewer/src/on_solana.rs"]
mod on_solana;
mod solana;
mod store;
mod vendor;

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use alloy::primitives::{Address, B256, Bytes, keccak256};
use alloy::rpc::types::Filter;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol_types::SolEvent;
use anyhow::{Context, Result, anyhow, bail};
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use chain::bindings::publishing::IPublishing;
use chain::bindings::strategies::{ChainNeed, IStrategyRegistry};
use chain::{Client, Deployments, Served};
use clap::Parser;
use shared::manifest::{Canonical, Manifest, hash_bytes, hash_to_bytes};
use shared::model::registry::{Job, JobState};
use shared::{Catalogue, Chain, Failure, ReviewAnswer, ReviewRequest, Upload, UploadAccepted};
use tokio::sync::Mutex;

#[derive(Parser)]
#[command(about = "Reviews, then compiles, submitted agents into signed bundles")]
struct Args {
    #[command(flatten)]
    chain_args: chain::ChainArgs,
    #[arg(long)]
    store: PathBuf,
    /// Where uploads are unpacked to be checked.
    #[arg(long)]
    builds: PathBuf,
    /// Where each job is compiled, in a directory made fresh for it. Cargo
    /// takes a `Cargo.toml` or a `.cargo` in any directory above a build for
    /// the build's own, so no build may be able to write here or above: it is
    /// made by the build service for itself alone, and a build finding either
    /// above it stops. By default, a directory of the build service's own in
    /// the system's temporary directory.
    #[arg(long, env = "SHONIN_BUILDER_JOBS")]
    jobs: Option<PathBuf>,
    /// The SDK this build service carries, which every submission is built
    /// against whatever it asked for: a copy of the SDK this program is
    /// built with, at its version, or the build service does not start.
    #[arg(long)]
    sdk: PathBuf,
    /// The vendored copy of the platform's reviewed crates (made with
    /// `--vendor-into`), the only crates.io crates a build can reach. The
    /// build service starts only if it holds exactly the allowlist.
    #[arg(long, env = "SHONIN_VENDOR")]
    vendor: PathBuf,
    /// The reviewer, which reads every submission before it is built.
    #[arg(long, env = "SHONIN_REVIEWER_URL")]
    reviewer_url: String,
    #[arg(long, default_value_t = 19_084)]
    port: u16,
    /// The address to listen on: this machine alone unless told otherwise.
    #[arg(long, env = "SHONIN_LISTEN", default_value = "127.0.0.1")]
    listen: std::net::IpAddr,
    /// How the build service's key is attested: `development`, by the role
    /// key and the development checker; `launcher`, by Confidential Space's
    /// launcher, with a key derived for this image by the key holder.
    #[arg(long, env = "SHONIN_ATTESTATION", value_enum, default_value_t = Attestation::Development)]
    attestation: Attestation,
    /// The key holder's status port, asked for this image's keys by
    /// attested exchange (launcher mode).
    #[arg(long, env = "SHONIN_HOLDER_URL")]
    holder_url: Option<String>,
    /// Where the build service's state is kept off the machine; without it,
    /// in the store as files only.
    #[arg(long, env = "SHONIN_STATE_URL")]
    state_url: Option<String>,
    /// Exit once idle this many seconds; without it, never.
    #[arg(long, env = "SHONIN_IDLE_EXIT_SECONDS")]
    idle_exit_seconds: Option<u64>,
    /// The launcher's token socket, which no build may reach.
    #[arg(long, hide = true, default_value = shonin_rootkey::launcher::LAUNCHER_SOCKET)]
    launcher_socket: PathBuf,
}

/// How the build service's key is attested.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Attestation {
    Development,
    Launcher,
}

/// How long an upload waits for its payment before it is forgotten: by the
/// clock, and in the chain time its payments have been read through.
const UNPAID_FOR: time::Duration = time::Duration::hours(24);

/// How long a forgotten upload is remembered, so a payment that arrives late
/// for it is still credited.
const FORGOTTEN_FOR: time::Duration = time::Duration::days(7);

/// The most uploads one wallet may have waiting for their payment.
const UNPAID_PER_WALLET: usize = 3;

/// The most uploads waiting for their payment, from everyone. Each holds an
/// archive of up to about 2 MB.
const UNPAID_AT_ALL: usize = 50;

/// The blocks in one page of payments. The chain client reads a page in
/// windows each endpoint takes: within the chain's configured log range, and
/// narrower at an endpoint that refuses a window as too wide.
const PAYMENT_SPAN: u64 = 2_000;

/// How many blocks deep a payment must be before it is read, on a chain whose
/// config does not say.
const CONFIRMATIONS: u64 = 12;

/// The first job id.
const FIRST_ID: u64 = 8_800;

/// The wallet an upload names, which alone may pay for it: an address on an
/// EVM chain, an ed25519 key on Solana. Written as each chain writes its own
/// (`0x` hex, or base58), so a queue written before Solana was served reads
/// as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Wallet {
    Evm(Address),
    Solana([u8; 32]),
}

impl Wallet {
    /// An upload's author as `chain` names a wallet.
    fn on(chain: Chain, text: &str) -> Result<Wallet, String> {
        if chain == Chain::Solana {
            text.trim()
                .parse::<chain::solana::Pubkey>()
                .map(|key| Wallet::Solana(key.to_bytes()))
                .map_err(|_| "the author is not a Solana address".to_string())
        } else {
            text.trim()
                .parse::<Address>()
                .map(Wallet::Evm)
                .map_err(|_| "the author is not an address".to_string())
        }
    }

    /// The wallet's own bytes, as an upload's reference is worked from.
    fn bytes(&self) -> Vec<u8> {
        match self {
            Wallet::Evm(address) => address.to_vec(),
            Wallet::Solana(key) => key.to_vec(),
        }
    }

    /// The EVM address, for a registration on an EVM chain.
    fn evm(&self) -> Result<Address> {
        match self {
            Wallet::Evm(address) => Ok(*address),
            Wallet::Solana(_) => bail!("a Solana wallet pays for nothing on an EVM chain"),
        }
    }

    /// The Solana key, for a registration on Solana.
    fn solana(&self) -> Result<chain::solana::Pubkey> {
        match self {
            Wallet::Solana(key) => Ok(chain::solana::Pubkey::new_from_array(*key)),
            Wallet::Evm(_) => bail!("an EVM wallet pays for nothing on Solana"),
        }
    }
}

impl From<Address> for Wallet {
    fn from(address: Address) -> Self {
        Wallet::Evm(address)
    }
}

impl std::fmt::Display for Wallet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Wallet::Evm(address) => write!(f, "{address:#x}"),
            Wallet::Solana(key) => {
                write!(f, "{}", chain::solana::Pubkey::new_from_array(*key))
            }
        }
    }
}

impl serde::Serialize for Wallet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Wallet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let chain = if text.starts_with("0x") {
            Chain::Base
        } else {
            Chain::Solana
        };
        Wallet::on(chain, &text).map_err(serde::de::Error::custom)
    }
}

struct Service {
    /// Every vault chain it builds for, connected yet or not: an agent is
    /// published on its vault chain.
    configured: Vec<Chain>,
    /// Those of them taken on so far (see [`take_on`]). A chain not yet taken
    /// on has its uploads turned away for now and its jobs left waiting.
    connected: std::sync::RwLock<BTreeMap<Chain, Arc<Served>>>,
    /// The Solana cluster, once taken on: the key blessed and stocked there
    /// (see [`solana::take_on`]).
    solana: std::sync::RwLock<Option<Arc<on_solana::OnSolana>>>,
    /// The build service's own Solana key, in the attested image.
    solana_key: Option<Arc<chain::solana::Keypair>>,
    /// How many blocks deep a payment must be before it is read, on each of
    /// `chains`.
    confirmations: BTreeMap<Chain, u64>,
    /// `local`, `testnet` or `mainnet`: which drivers are on offer.
    network: String,
    catalogue: Catalogue,
    store: PathBuf,
    builds: PathBuf,
    jobs: PathBuf,
    sdk: PathBuf,
    vendor: PathBuf,
    /// The crates.io crates a project may depend on.
    allowed: shared::dependencies::Allowlist,
    reviewer: String,
    http: reqwest::Client,
    /// The queue.
    queue: Mutex<Queue>,
    /// Where the queue is kept, so restarting the service does not lose a
    /// job someone paid for.
    queue_file: PathBuf,
    /// Where each unfinished job's archive is kept, a file to each, so the
    /// queue written down on every change holds no archive.
    archives: PathBuf,
    /// The next block to read payments from, on each chain by name.
    payments_read: Mutex<BTreeMap<String, u64>>,
    /// Where that is kept, so a restart reads on from where it left off.
    payments_file: PathBuf,
    /// On each chain, the time of the last block whose payments have all been
    /// read and acted on. Not kept: after a restart nothing is forgotten
    /// until a page has been read again.
    payments_read_to: Mutex<BTreeMap<Chain, time::PrimitiveDateTime>>,
    /// A `Cargo.toml` or `.cargo` found above the jobs directory, which cargo
    /// would take for every build's own (see [`held_by`]): while there is
    /// one, no job is taken from the queue, and the health route says so.
    held: std::sync::Mutex<Option<PathBuf>>,
    /// Why builds can't be kept from the launcher's socket, if they can't
    /// (see [`isolation`]): while so, no job is taken, and the health route
    /// says why.
    exposed: Option<String>,
    /// The chain id of the canonical chain, whose registry the API reads to
    /// count the key blessed: the API takes no wake plan until it is.
    canonical: Option<u64>,
    /// The image the build service runs, as the launcher attests it, in the
    /// attested image: what the manifest names as the builder's image.
    image: Option<B256>,
    /// The build service's own key, in the attested image; the role key the
    /// node holds otherwise.
    key: Option<PrivateKeySigner>,
    /// The key's attestation, in the attested image.
    attestation: Option<Arc<dyn cloud::Attesting>>,
    /// Where the build service's state is kept off the machine, if it is,
    /// and where its built files go into the API's store.
    internal: Option<cloud::Internal>,
    /// On each chain, by chain id, how many payments the registry had taken
    /// at the last block payments were read through.
    payments_seen: Mutex<BTreeMap<String, u64>>,
    /// The chains, by chain id, whose last turn of reading payments failed,
    /// and when each is read again (see [`Stalled`]).
    payments_stalled: std::sync::Mutex<BTreeMap<String, Stalled>>,
    /// When the build service last had anything to do.
    idle: cloud::Idle,
    /// The work loops' turns, closed before the state is written at exit.
    turns: cloud::Turns,
    /// The sweep a job's end started, apart from the runtime, until it is
    /// waited for (see [`Fresh`]).
    sweeping: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Service {
    /// Whether the store holds a bundle's files. Kept off the machine, a
    /// bundle's files are pushed to the API's store before it is
    /// registered, so one the registry holds is in the API's store.
    fn holds(&self, bundle_hash: &str) -> bool {
        self.internal.is_some() || store::holds_bundle(&self.store, bundle_hash)
    }

    /// Keep `bytes` in the file at `path`, and when the state is kept off
    /// the machine, as the blob `name` too: an error unless both are kept.
    async fn keep(&self, path: &Path, name: &str, bytes: &[u8]) -> Result<()> {
        write_atomically(path, bytes)?;
        if let Some(internal) = &self.internal {
            internal.write_state(name, bytes).await?;
        }
        Ok(())
    }

    /// Let go of the archives of these jobs off the machine too. Not waited
    /// for: an archive left behind is only space.
    fn let_go(&self, ids: Vec<String>) {
        // Before the API takes the build service's writes, they are left.
        let Some(internal) = self
            .internal
            .as_ref()
            .filter(|internal| internal.writable())
        else {
            return;
        };
        for id in ids {
            let name = archive_name(&id);
            let internal = internal.clone();
            tokio::spawn(async move {
                if let Err(error) = internal.remove_state(&name).await {
                    tracing::warn!(
                        name,
                        error = format!("{error:#}"),
                        "an archive kept off the machine could not be removed"
                    );
                }
            });
        }
    }

    /// [`sweep_archives`], here and off the machine.
    fn sweep(&self, jobs: &[Entry]) {
        let removed = sweep_archives(&self.archives, jobs);
        self.let_go(removed);
    }

    /// Whether the build service has anything to do: a paid job waiting, being
    /// reviewed or built; a blessing due; or a payment on a chain past those
    /// read (`unread`).
    async fn busy(&self, unread: bool) -> bool {
        unread
            || self.attestation.as_ref().is_some_and(|a| a.busy())
            || self.queue.lock().await.jobs.iter().any(|entry| {
                matches!(
                    entry.job.state,
                    JobState::Queued { .. } | JobState::Reviewing | JobState::Building
                )
            })
    }

    /// Whether the last turn of reading payments on the chain `chain_id`
    /// failed, and if so when it is read again.
    fn stalled_on(&self, chain_id: &str) -> Option<Stalled> {
        self.payments_stalled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(chain_id)
            .copied()
    }

    /// Whether the API would take the wake plan: in the attested image, once
    /// the key is blessed on the canonical chain; anywhere else, always.
    fn plan_taken(&self) -> bool {
        match (&self.attestation, self.canonical) {
            (Some(attestation), Some(chain_id)) => attestation.ready(chain_id),
            _ => true,
        }
    }

    /// Whether the state kept off the machine can be written, and is this
    /// build service's own: the key is blessed, and whatever was found
    /// sealed under another key is set aside (see [`settle`]). Until then no
    /// upload is taken and no job is started, since neither could be kept.
    fn keeps_state(&self) -> bool {
        self.unkept().is_none()
    }

    /// Why the state kept off the machine can't be kept yet (see
    /// [`Service::keeps_state`]), or `None` once it can.
    fn unkept(&self) -> Option<&'static str> {
        let internal = self.internal.as_ref()?;
        if !internal.writable() {
            Some("its key is not blessed yet")
        } else if !internal.settled() {
            Some("the state found sealed under another key is not set aside yet")
        } else {
            None
        }
    }

    /// Wait for the sweep a job's end started, if one is under way.
    async fn swept(&self) {
        let sweeping = self
            .sweeping
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(sweeping) = sweeping {
            let _ = sweeping.await;
        }
    }

    /// Whether `chain` is served and taken on: its uploads are taken and its
    /// jobs built.
    fn connected_to(&self, chain: Chain) -> bool {
        if chain == Chain::Solana {
            return self.on_solana().is_ok();
        }
        self.on(chain).is_ok()
    }

    /// The Solana cluster, once taken on.
    fn on_solana(&self) -> Result<Arc<on_solana::OnSolana>> {
        if !self.configured.contains(&Chain::Solana) {
            return Err(anyhow!("this build service does not serve solana"));
        }
        self.solana
            .read()
            .ok()
            .and_then(|solana| solana.clone())
            .ok_or_else(|| anyhow!("solana is not taken on yet"))
    }

    fn on(&self, chain: Chain) -> Result<Arc<Served>> {
        if !self.configured.contains(&chain) {
            return Err(anyhow!("this build service does not serve {chain}"));
        }
        self.connected
            .read()
            .ok()
            .and_then(|connected| connected.get(&chain).cloned())
            .ok_or_else(|| anyhow!("{chain} is not connected yet"))
    }

    /// Every vault chain it serves, connected yet or not: a chain whose node
    /// is down is still served, and nothing on it is forgotten while its
    /// payments can't be read.
    fn served(&self) -> Vec<Chain> {
        self.configured.clone()
    }
}

/// The queue as it is kept on disk.
#[derive(serde::Serialize, serde::Deserialize)]
struct Queue {
    /// The id the next upload is given. Ids are never given twice, though an
    /// unpaid upload is forgotten.
    next_id: u64,
    /// Every job, oldest first.
    jobs: Vec<Entry>,
    /// The uploads forgotten in the last week, so a payment that comes for
    /// one late is not lost.
    #[serde(default)]
    forgotten: Vec<Forgotten>,
    /// Payments for forgotten uploads, each waiting for the same upload to be
    /// sent again. Kept until used.
    #[serde(default)]
    credits: Vec<Credit>,
    /// Whether the last write of the queue failed, so what is in memory may
    /// be more than what is on disk.
    #[serde(skip)]
    unwritten: bool,
    /// Payments already said to admit nothing, by chain and id, so one read
    /// again is not said again.
    #[serde(skip)]
    unmatched: BTreeSet<(Chain, u64)>,
}

impl Default for Queue {
    fn default() -> Self {
        Queue {
            next_id: FIRST_ID,
            jobs: Vec::new(),
            forgotten: Vec::new(),
            credits: Vec::new(),
            unwritten: false,
            unmatched: BTreeSet::new(),
        }
    }
}

/// A queue as written down: now, or as it was before it counted its ids, a
/// bare list.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Written {
    Queue(Queue),
    Jobs(Vec<Entry>),
}

impl From<Written> for Queue {
    fn from(written: Written) -> Self {
        match written {
            Written::Queue(queue) => queue,
            Written::Jobs(jobs) => Queue {
                next_id: FIRST_ID + jobs.len() as u64,
                jobs,
                ..Queue::default()
            },
        }
    }
}

/// A job, and what it was asked to build.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Entry {
    job: Job,
    /// The reference the author pays against.
    upload: B256,
    /// The wallet the upload named: only its payment starts the job.
    author: Wallet,
    /// The project, base64, as a queue written before each archive had a
    /// file of its own held it: read once, moved to its file, and never
    /// written again.
    #[serde(default, skip_serializing)]
    archive: String,
    /// Where the agent's `shonin.toml` is, inside the project.
    manifest_path: String,
    /// The cargo package to build.
    package: String,
    /// How many times it has been taken from the queue to be reviewed and
    /// built: more than once only when a restart caught it mid-way.
    #[serde(default)]
    attempts: u32,
    /// How many times in a row it has gone back in the queue because the
    /// reviewer did not answer, and the earliest it is taken again (see
    /// [`waiting_for_the_reviewer`]).
    #[serde(default, skip_serializing_if = "is_zero")]
    reviewer_away: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    not_before: Option<time::PrimitiveDateTime>,
}

fn is_zero(count: &u32) -> bool {
    *count == 0
}

/// The most times a job is taken from the queue (see [`picked_up`]). One a
/// restart caught mid-way that often is taken to be what brought the build
/// service down, and has failed, so the queue moves on past it.
const MOST_ATTEMPTS: u32 = 3;

/// What is remembered of an upload forgotten before it was paid for: enough
/// to know a late payment for it, and the upload when it is sent again, but
/// not the archive.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Forgotten {
    /// The reference it was given to pay against.
    upload: B256,
    chain: Chain,
    author: Wallet,
    source_hash: String,
    sealed: bool,
    manifest_path: String,
    package: String,
    /// When it was forgotten.
    at: time::PrimitiveDateTime,
}

impl Forgotten {
    /// What is remembered of `entry`, forgotten `at`.
    fn of(entry: &Entry, at: time::PrimitiveDateTime) -> Self {
        Forgotten {
            upload: entry.upload,
            chain: entry.job.chain,
            author: entry.author,
            source_hash: entry.job.source_hash.clone(),
            sealed: entry.job.sealed,
            manifest_path: entry.manifest_path.clone(),
            package: entry.package.clone(),
            at,
        }
    }

    /// Whether an upload is this one again: the same author, chain and
    /// source, built the same way.
    fn is(
        &self,
        chain: Chain,
        author: Wallet,
        source_hash: &str,
        sealed: bool,
        manifest_path: &str,
        package: &str,
    ) -> bool {
        self.chain == chain
            && self.author == author
            && self.source_hash == source_hash
            && self.sealed == sealed
            && self.manifest_path == manifest_path
            && self.package == package
    }
}

/// A payment for an upload already forgotten, or already paid for, which
/// admits the same upload when it is sent again.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Credit {
    /// The upload it paid for.
    paid_for: Forgotten,
    /// The id the payment took.
    strategy_id: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(chain::log_filter())
        .with_target(false)
        .init();

    // A component a build made is loaded by this same program, in a process
    // of its own, which says only whether it loads (see `build::load_apart`).
    // The compiler is run under it too, so that nothing a build started
    // outlives the service (see `build::watch_here`).
    let mut given = std::env::args_os().skip(1);
    match given.next() {
        Some(first) if first == build::LOAD => {
            let loads = given
                .next()
                .is_some_and(|wasm| build::load_here(Path::new(&wasm)));
            std::process::exit(if loads { 0 } else { 1 });
        }
        Some(first) if first == build::WATCH => std::process::exit(build::watch_here(given)),
        Some(first) if first == isolation::SWEEP => std::process::exit(isolation::sweep_here()),
        Some(first) if first == isolation::PROBE => {
            let socket = given.next().context("--probe-socket names a socket")?;
            std::process::exit(isolation::probe_here(Path::new(&socket)));
        }
        // Made once, when the image is made, with the network it has then.
        Some(first) if first == vendor::INTO => {
            let into = given.next().context("--vendor-into names a directory")?;
            let allowed = shared::dependencies::Allowlist::platform();
            vendor::fetch(&allowed, Path::new(&into)).await?;
            tracing::info!(
                crates = allowed.crates().len(),
                "the reviewed crates are vendored"
            );
            return Ok(());
        }
        _ => {}
    }

    let args = Args::parse();
    // A lock may name the SDK from crates.io at the version this build
    // service carries, which is this program's own SDK: the copy it builds
    // against must be that version too.
    let sdk_dependencies = build::carries(&args.sdk).map_err(|error| anyhow!(error))?;
    let allowed = shared::dependencies::Allowlist::platform()
        .carrying(strategy_sdk::VERSION, sdk_dependencies)
        .map_err(|error| anyhow!(error))?;
    vendor::check(&allowed, &args.vendor)?;
    let jobs = prepare_jobs(args.jobs.clone())?;
    let launcher = args.attestation == Attestation::Launcher;
    if launcher && args.holder_url.is_none() {
        bail!("launcher mode needs the key holder: set SHONIN_HOLDER_URL");
    }
    let launched = match (launcher, &args.holder_url) {
        (true, Some(holder)) => Some(
            cloud::launch(
                shonin_rootkey::ImageKind::Builder,
                holder,
                &args.launcher_socket,
            )
            .await?,
        ),
        _ => None,
    };
    // The image the manifests name as the builder's: the one the launcher
    // attests this machine runs.
    let image = match &launched {
        Some(launched) => {
            let image = cloud::attested_image(&args.launcher_socket, launched.key.address())
                .await
                .context("the image this machine runs, from the launcher's token")?;
            tracing::info!(%image, "the image the launcher attests");
            Some(image)
        }
        None => None,
    };
    let targets = args.chain_args.targets()?;
    // The network's root chain, or base where it names none, as the API
    // takes it.
    let canonical = args
        .chain_args
        .root_chain()?
        .and_then(|key| key.evm_chain_id())
        .or_else(|| {
            targets
                .iter()
                .find(|target| target.vault_chain == Chain::Base)
                .and_then(|target| target.key.evm_chain_id())
        });
    // In the attested image the API takes the build service's writes only
    // once its key is blessed on the canonical chain: none is sent before.
    let internal = match &args.state_url {
        Some(url) => Some(match &launched {
            Some(launched) => cloud::Internal::new(
                url,
                "builder",
                cloud::Signer::Key(launched.key.clone()),
                launched.sealing.clone(),
            )?
            .writes_once({
                let attestation = launched.attestation.clone();
                move || canonical.is_none_or(|chain_id| attestation.ready(chain_id))
            }),
            None => {
                cloud::development(url, "builder", &args.chain_args, |roles| roles.builder).await?
            }
        }),
        None => None,
    };
    // Nothing of the build user's from before this start is left running,
    // and in the attested image the launcher's socket, which may come open to
    // every user, is closed to all but root; then builds are checked against
    // it (see [`isolation::at_start`]).
    let (closing, reach) = isolation::at_start(
        &args.launcher_socket,
        &[&jobs, &args.builds, &args.sdk, &args.vendor].map(PathBuf::as_path),
        launcher,
    )
    .await;
    let exposed = isolation::holds_the_queue(&reach, launcher).map(|why| {
        if closing.failed.is_empty() {
            why
        } else {
            format!("{why}; closing it failed: {}", closing.failed.join("; "))
        }
    });
    match &exposed {
        Some(why) => tracing::error!(
            why,
            "builds can't be kept from the launcher's socket: no job is taken"
        ),
        None => {
            tracing::info!(?reach, user = ?isolation::build_user(), "builds are kept from the launcher's socket")
        }
    }

    // A job someone paid for should survive this service restarting, so the
    // queue is on disk rather than only in memory. One that is there but
    // can't be read stops the service: carrying on with an empty queue would
    // lose every paid job and give their ids out again.
    let queue_file = args.store.join("queue.json");
    let archives = args.store.join("archives");
    let payments_file = args.store.join("payments.json");
    // Kept off the machine, what was kept is brought back first, the
    // archives of the jobs still unfinished with it.
    if let Some(internal) = &internal {
        restore(internal, &queue_file, &payments_file, &archives).await?;
    }
    let mut queue: Queue = read_kept::<Written>(&queue_file)?
        .map(Queue::from)
        .unwrap_or_default();
    let payments_read: BTreeMap<String, u64> = read_kept(&payments_file)?.unwrap_or_default();

    let network = args.chain_args.network.to_string();
    let confirmations = targets
        .iter()
        .map(|target| {
            (
                target.vault_chain,
                target.config.confirmations.unwrap_or(CONFIRMATIONS),
            )
        })
        .collect();
    let mut configured: Vec<Chain> = targets.iter().map(|target| target.vault_chain).collect();
    let solana_targets = args.chain_args.solana_targets()?;
    configured.extend(solana_targets.iter().map(|target| target.vault_chain));
    // Each chain is taken on as soon as it connects: one out of reach (its
    // node down, its deployment missing) is tried again in the background,
    // and doesn't keep the build service from the others.
    let mut connecting = args.chain_args.connect_each()?;
    let mut clusters = if solana_targets.is_empty() {
        tokio::sync::mpsc::unbounded_channel().1
    } else {
        args.chain_args.connect_each_solana()?
    };

    let moved = move_archives(&archives, &mut queue.jobs)?;
    let swept = sweep_archives(&archives, &queue.jobs);

    let service = Arc::new(Service {
        configured,
        connected: Default::default(),
        solana: Default::default(),
        solana_key: launched.as_ref().map(|launched| launched.solana.clone()),
        confirmations,
        network,
        catalogue: Catalogue::published(),
        store: args.store,
        builds: args.builds,
        jobs,
        sdk: args.sdk,
        vendor: args.vendor,
        allowed,
        reviewer: args.reviewer_url.trim_end_matches('/').to_string(),
        http: reqwest::Client::new(),
        queue: Mutex::new(queue),
        queue_file,
        archives,
        payments_read: Mutex::new(payments_read),
        payments_file,
        payments_read_to: Mutex::new(BTreeMap::new()),
        held: std::sync::Mutex::new(None),
        exposed,
        canonical,
        image,
        key: launched.as_ref().map(|launched| launched.key.clone()),
        attestation: launched
            .as_ref()
            .map(|launched| launched.attestation.clone()),
        internal,
        payments_seen: Mutex::new(BTreeMap::new()),
        payments_stalled: Default::default(),
        idle: cloud::Idle::new(args.idle_exit_seconds.map(Duration::from_secs)),
        turns: cloud::Turns::default(),
        sweeping: std::sync::Mutex::new(None),
    });
    service.let_go(swept);
    if let Some(key) = &service.key {
        tracing::info!(key = %key.address(), "the build service's own key, derived for this image");
    }
    // Not written yet, it is written once it can be (see [`settle`]).
    if moved {
        let _ = write_queue(&service, &mut *service.queue.lock().await).await;
    }

    let joining = service.clone();
    tokio::spawn(async move {
        while let Some(served) = connecting.recv().await {
            tokio::spawn(take_on(joining.clone(), Arc::new(served)));
        }
    });
    let joining = service.clone();
    tokio::spawn(async move {
        while let Some(served) = clusters.recv().await {
            tokio::spawn(solana::take_on(joining.clone(), served));
        }
    });

    let worker = service.clone();
    tokio::spawn(async move {
        loop {
            let Some(turn) = worker.turns.turn().await else {
                return;
            };
            if let Err(error) = take_one(&worker).await {
                tracing::warn!(%error, "the queue stalled");
            }
            drop(turn);
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    let attested = service
        .attestation
        .as_ref()
        .map(|attestation| attestation.routes())
        .unwrap_or_default();
    let app = axum::Router::new()
        .route("/health", get(health))
        .route("/jobs", get(list_jobs))
        .route("/jobs/{id}", get(one_job))
        .route(
            "/{chain}/uploads",
            post(upload).layer(axum::extract::DefaultBodyLimit::max(
                shared::archive::MOST_UPLOAD_BYTES,
            )),
        )
        .with_state(service.clone())
        .merge(attested);

    let address = SocketAddr::from((args.listen, args.port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "the build service is open");
    let stopping = Arc::new(tokio::sync::Notify::new());
    let stop = stopping.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { stop.notified().await })
            .await
    });
    watch_idle(&service).await;
    // Idle: stop listening, let what is being answered finish, end the
    // reading of payments and the taking of jobs, write down the queue and
    // the wake plan, and exit.
    stopping.notify_one();
    let _ = server.await;
    service.turns.close().await;
    service.swept().await;
    match write_queue(&service, &mut *service.queue.lock().await).await {
        // Before the blessing nothing has changed that the API would keep.
        Err(error) if error.is::<cloud::NotBlessed>() => tracing::info!("{error}"),
        written => written?,
    }
    // Busy as it stands now: a payment or a job that came in while closing
    // has the waker start the build service again.
    let unread = unread_payments(&service).await;
    let busy = service.busy(unread.now).await;
    if busy {
        tracing::info!("work came in while closing: the wake plan says busy");
    }
    if let Some(internal) = &service.internal
        && service.plan_taken()
    {
        match internal
            .put_plan(&plan(&service, busy, &unread).await)
            .await
        {
            Err(error) if error.is::<cloud::NotBlessed>() => tracing::info!("{error}"),
            told => told?,
        }
    }
    tracing::info!("idle: the build service exits");
    Ok(())
}

/// Bring back what was kept off the machine into the files the build
/// service reads at start: the queue, where payments were read to, and the
/// archive of every job the queue holds unfinished. What is not kept there
/// leaves the file as it is.
async fn restore(
    internal: &cloud::Internal,
    queue_file: &Path,
    payments_file: &Path,
    archives: &Path,
) -> Result<()> {
    for (name, file) in [(QUEUE, queue_file), (PAYMENTS, payments_file)] {
        if let Some(bytes) = internal.read_state(name).await? {
            write_atomically(file, &bytes)
                .with_context(|| format!("bringing back {}", file.display()))?;
        }
    }
    let Some(queue) = read_kept::<Written>(queue_file)?.map(Queue::from) else {
        return Ok(());
    };
    let mut brought = 0;
    for entry in queue
        .jobs
        .iter()
        .filter(|entry| !entry.job.state.is_finished())
    {
        let name = archive_name(&entry.job.id);
        match internal.read_state(&name).await? {
            Some(bytes) => {
                write_atomically(&archive_file(archives, &entry.job.id), &bytes)?;
                brought += 1;
            }
            None => tracing::warn!(id = entry.job.id, "an unfinished job's archive is not kept"),
        }
    }
    tracing::info!(
        jobs = queue.jobs.len(),
        archives = brought,
        "brought the queue back"
    );
    Ok(())
}

/// The names the queue and where payments were read to are kept under off
/// the machine.
const QUEUE: &str = "queue";
const PAYMENTS: &str = "payments";

/// The name a job's archive is kept under off the machine.
fn archive_name(id: &str) -> String {
    format!("archive-{id}")
}

/// The build service's wake plan.
async fn plan(service: &Service, busy: bool, unread: &Unread) -> cloud::WakePlan {
    cloud::WakePlan {
        busy,
        renew_at: service.attestation.as_ref().and_then(|a| a.renew_at()),
        retry_at: unread.retry_at,
        // A chain whose payments can't be read for now is left out, so a
        // payment there doesn't have the waker start the build service over
        // and over: the retry time starts it instead.
        payments_seen: service
            .payments_seen
            .lock()
            .await
            .iter()
            .filter(|(chain_id, _)| !unread.stalled.contains(*chain_id))
            .map(|(chain_id, &count)| (chain_id.clone(), count))
            .collect(),
    }
}

/// What the build service has of payments it has not read yet, on the
/// chains connected (see [`unread_payments`]).
#[derive(Debug, Default)]
struct Unread {
    /// A payment it has not read waits on a chain whose payments are read
    /// now or soon: work in hand.
    now: bool,
    /// The chains, by chain id, where a payment may wait unread while their
    /// reads fail: each is left out of the wake plan's counts.
    stalled: BTreeSet<String>,
    /// When the first of those chains is read again, in unix seconds: the
    /// waker starts the build service then, if it has exited.
    retry_at: Option<u64>,
}

/// What a chain connected has taken of payments the build service has not
/// read yet: its registry's count now past the count at the last block read
/// through. On a chain whose reads are failing ([`Stalled`]), a payment that
/// may wait (the count past the one seen, or not to be had) is work in hand
/// only while it is read again soon and its last read failed for no reason
/// of the registry's own; otherwise it gives the plan a retry time. A chain
/// whose reads are not failing and whose count can't be read now holds
/// nothing up.
async fn unread_payments(service: &Service) -> Unread {
    let mut unread = Unread::default();
    if let Ok(on) = service.on_solana()
        && solana::unread(service, &on).await
    {
        unread.now = true;
    }
    let connected: Vec<Arc<Served>> = service
        .connected
        .read()
        .map(|connected| connected.values().cloned().collect())
        .unwrap_or_default();
    for on in connected {
        let chain_id = on.client.chain_id.to_string();
        let stalled = service.stalled_on(&chain_id);
        let seen = service.payments_seen.lock().await.get(&chain_id).copied();
        let waiting = match seen {
            // Not read through even once yet.
            None => true,
            Some(seen) => match on
                .client
                .read(on.deployments.publishing, IPublishing::countCall {})
                .await
            {
                Ok(count) => count > alloy::primitives::U256::from(seen),
                Err(error) => {
                    tracing::debug!(chain = %on.vault_chain, error = format!("{error:#}"), "the payments taken could not be counted");
                    stalled.is_some()
                }
            },
        };
        if waiting {
            unread.waiting(chain_id, stalled, unix_now());
        }
    }
    unread
}

impl Unread {
    /// Note a payment that may wait unread on the chain `chain_id`, whose
    /// reads are failing as `stalled` says, at `now` (unix seconds).
    fn waiting(&mut self, chain_id: String, stalled: Option<Stalled>, now: u64) {
        let Some(stalled) = stalled else {
            self.now = true;
            return;
        };
        if !stalled.paused && stalled.retry_at <= now + STALLED_KEEPS_UP.as_secs() {
            self.now = true;
        }
        self.stalled.insert(chain_id);
        self.retry_at = Some(
            self.retry_at
                .map_or(stalled.retry_at, |at| at.min(stalled.retry_at)),
        );
    }
}

/// Watch whether the build service has anything to do, telling the API's
/// waker each time its plan changes, and return once it has been idle long
/// enough to exit (never, without an idle exit). No plan is sent until the
/// API would take it (see [`Service::plan_taken`]); one it refuses for want
/// of the blessing is said once, at info, and sent again next time round.
async fn watch_idle(service: &Service) {
    let mut told = None;
    let mut said_not_blessed = false;
    let mut said_waiting = false;
    let mut said_kept = false;
    loop {
        settle(service).await;
        if service.internal.is_some() && service.keeps_state() && !said_kept {
            tracing::info!(
                "the key is blessed and the state kept is the build service's own: uploads and jobs are taken"
            );
            said_kept = true;
        }
        let unread = unread_payments(service).await;
        let busy = service.busy(unread.now).await;
        let plan = plan(service, busy, &unread).await;
        let taken = service.plan_taken();
        if service.internal.is_some() && !taken && !said_waiting {
            tracing::info!(
                chain_id = service.canonical,
                "the wake plan is sent once the key is blessed on the canonical chain"
            );
            said_waiting = true;
        }
        if let Some(internal) = &service.internal
            && told.as_ref() != Some(&plan)
            && taken
        {
            match internal.put_plan(&plan).await {
                Ok(()) => told = Some(plan),
                Err(error) if error.is::<cloud::NotBlessed>() => {
                    if !said_not_blessed {
                        tracing::info!("{error}");
                        said_not_blessed = true;
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        error = format!("{error:#}"),
                        "the wake plan could not be told"
                    )
                }
            }
        }
        if service.idle.should_exit(busy) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// Once the API takes the build service's writes, set aside what was found
/// sealed under another key, and write the queue down if its last write
/// failed. What fails is said and tried again next time round.
async fn settle(service: &Service) {
    let Some(internal) = &service.internal else {
        return;
    };
    if !internal.writable() {
        return;
    }
    if !internal.settled()
        && let Err(error) = internal.set_aside().await
    {
        tracing::warn!(
            error = format!("{error:#}"),
            "the state sealed under another key could not be set aside; trying again"
        );
    }
    let mut queue = service.queue.lock().await;
    if queue.unwritten {
        let _ = write_queue(service, &mut queue).await;
    }
}

/// The health route: 503 while the state kept off the machine can't be kept
/// yet (see [`Service::keeps_state`]), since no upload nor job is taken
/// meanwhile, saying why; while the queue is held (see [`held_by`]), naming
/// what holds it, while the last sweep of the build user's processes was
/// not clean (see [`isolation::unswept`]), and while a job has gone back in the queue more than
/// [`MOST_REVIEWER_AWAY`] times in a row for want of a verdict (it still
/// waits, and is asked again), naming the jobs.
async fn health(State(service): State<Arc<Service>>) -> (StatusCode, String) {
    if let Some(why) = &service.exposed {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("builder: taking no jobs: {why}"),
        );
    }
    if let Some(why) = isolation::unswept() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "builder: taking no jobs until the build user's processes are all killed: {why}"
            ),
        );
    }
    if let Some(why) = service.unkept() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("builder: taking no uploads and no jobs until its state can be kept: {why}"),
        );
    }
    let held = service
        .held
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(found) = held {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "builder: holding the queue: {} lies above the jobs directory",
                found.display()
            ),
        );
    }
    let unreviewed: Vec<String> = service
        .queue
        .lock()
        .await
        .jobs
        .iter()
        .filter(|entry| {
            matches!(entry.job.state, JobState::Queued { .. })
                && entry.reviewer_away > MOST_REVIEWER_AWAY
        })
        .map(|entry| format!("{} ({} times)", entry.job.id, entry.reviewer_away))
        .collect();
    if !unreviewed.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "builder: the reviewer gave no verdict, asked again and again, for {}: the \
                 jobs wait in the queue and are asked again",
                unreviewed.join(", ")
            ),
        );
    }
    (StatusCode::OK, "builder".to_string())
}

/// How many times in a row a job may go back in the queue for want of a
/// verdict from the reviewer before the health route says so.
const MOST_REVIEWER_AWAY: u32 = 3;

/// A `Cargo.toml` or a `.cargo` in the jobs directory or any directory above
/// it, or `None`. Cargo takes either for every build's own (see
/// [`build::above`]), and no build of anyone's should run with it: a job is
/// not to blame for it, so rather than fail every job taken while it is
/// there, none is taken. The queue is held, each job keeping its place and
/// its payment, and the health route answers 503, until it is looked into and
/// removed.
fn held_by(jobs: &Path) -> Option<PathBuf> {
    build::above(&jobs.join("job"))
}

/// Note whether the queue is held by `found`, saying so as it changes: at
/// error, naming the file, when the queue is first held, and when it is let
/// go.
fn hold(service: &Service, found: Option<PathBuf>) {
    let mut held = service.held.lock().unwrap_or_else(|e| e.into_inner());
    if *held == found {
        return;
    }
    match &found {
        Some(found) => tracing::error!(
            found = %found.display(),
            "a Cargo.toml or a Cargo config lies above the jobs directory, where cargo would \
             take it for every build's own: no job is taken from the queue until it is looked \
             into and removed"
        ),
        None => tracing::info!("nothing lies above the jobs directory any more: taking jobs again"),
    }
    *held = found;
}

/// Make the directory jobs are compiled in, for this service alone, and
/// clear what a build caught by a restart left there.
fn prepare_jobs(given: Option<PathBuf>) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let jobs = given.unwrap_or_else(|| {
        std::env::temp_dir().join(format!(
            "shonin-builder-jobs-{}",
            rustix::process::getuid().as_raw()
        ))
    });
    std::fs::create_dir_all(&jobs)
        .with_context(|| format!("making the jobs directory {}", jobs.display()))?;
    let found = std::fs::symlink_metadata(&jobs)?;
    if !found.is_dir() {
        bail!("the jobs directory {} is not a directory", jobs.display());
    }
    // Builds running as a user of their own must pass through it to their
    // own job directories, and may list and write nothing in it.
    let mode = if isolation::build_user().is_some() {
        0o711
    } else {
        0o700
    };
    std::fs::set_permissions(&jobs, std::fs::Permissions::from_mode(mode))?;
    for entry in std::fs::read_dir(&jobs)? {
        let path = entry?.path();
        let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
    }
    Ok(jobs)
}

/// A file this service keeps, read back: `None` if there is none yet, and an
/// error if there is one that can't be read.
fn read_kept<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("{} could not be read", path.display()));
        }
    };
    serde_json::from_slice(&bytes).map(Some).with_context(|| {
        format!(
            "{} is there but is not whole, so the build service is not starting rather than \
             lose what it holds: restore it, or a backup of it, with queue.json and \
             payments.json from the same moment. payments.json must never be ahead of \
             queue.json, or a payment it has read past is never read again",
            path.display()
        )
    })
}

/// The queue as a restart finds it, made ready to carry on. A job that was
/// being reviewed or built when the service stopped goes back in the queue:
/// the service stopping is not the author's fault, and a job left mid-way
/// would hold every later job of theirs for good. A failed job's $1 being
/// spent is about a build that failed, not about this process stopping
/// while it ran. One whose registration landed before the service stopped,
/// named in `registered` with the bundle the registry holds, is never
/// reviewed or built again: it is ready with that bundle if the store holds
/// its files (`stored`), and failed otherwise, never ready with a bundle
/// nobody can read.
///
/// One taken from the queue [`MOST_ATTEMPTS`] times already, each ended by a
/// restart, has failed rather than go back in the queue: a build that takes
/// the build service down with it would otherwise be built after every
/// restart, for good, and nothing behind it ever would.
///
/// Only the jobs on `chain` are looked at: each chain's are picked back up
/// once it is connected, and the jobs on a chain not yet connected are left
/// as they are, never taken from the queue meanwhile.
fn picked_up(
    jobs: &mut [Entry],
    chain: Chain,
    registered: &BTreeMap<String, String>,
    stored: impl Fn(&str) -> bool,
) {
    for entry in jobs {
        if entry.job.chain != chain
            || !matches!(entry.job.state, JobState::Reviewing | JobState::Building)
        {
            continue;
        }
        if let Some(bundle_hash) = registered.get(&entry.job.id) {
            if stored(bundle_hash) {
                tracing::info!(
                    id = entry.job.id,
                    "a job the restart interrupted was registered already, so it is ready"
                );
                entry.job.state = JobState::Ready;
                entry.job.bundle_hash = Some(bundle_hash.clone());
            } else {
                tracing::warn!(
                    id = entry.job.id,
                    bundle_hash,
                    "a job the restart interrupted was registered already, with a bundle the \
                     store does not hold, so it has failed"
                );
                entry.job.state = JobState::Failed {
                    outcome: Failure::Service,
                };
            }
        } else if entry.attempts >= MOST_ATTEMPTS {
            tracing::warn!(
                id = entry.job.id,
                attempts = entry.attempts,
                "a job a restart interrupted has been every time it was taken from the queue, so it \
                 is taken to be what brings the build service down: it has failed"
            );
            entry.job.state = JobState::Failed {
                outcome: Failure::Service,
            };
        } else {
            tracing::info!(
                id = entry.job.id,
                "a job the restart interrupted is queued again"
            );
            entry.job.state = JobState::Queued { position: 0 };
        }
    }
}

/// The jobs on `on` a restart caught mid-way whose strategy the registry
/// already holds, by job id, each with the bundle registered for it.
async fn registered_before_the_restart(
    on: &Served,
    jobs: &[Entry],
) -> Result<BTreeMap<String, String>> {
    let mut registered = BTreeMap::new();
    for entry in jobs {
        if entry.job.chain != on.vault_chain
            || !matches!(entry.job.state, JobState::Reviewing | JobState::Building)
        {
            continue;
        }
        let Some(strategy_id) = entry.job.strategy_id else {
            continue;
        };
        if let Some(bundle_hash) = registered_bundle(on, strategy_id).await? {
            registered.insert(entry.job.id.clone(), bundle_hash);
        }
    }
    Ok(registered)
}

/// The bundle the registry holds for a strategy, if it is registered: a
/// registration always names a bundle, and nothing else does.
async fn registered_bundle(on: &Served, strategy_id: u64) -> Result<Option<String>> {
    let bundle = on
        .client
        .read(
            on.deployments.strategies,
            IStrategyRegistry::bundleHashOfCall {
                strategyId: alloy::primitives::U256::from(strategy_id),
            },
        )
        .await?;
    Ok((bundle != B256::ZERO).then(|| format!("sha256:{}", alloy::hex::encode(bundle))))
}

/// Where a job's archive is kept until the job ends.
fn archive_file(archives: &Path, id: &str) -> PathBuf {
    archives.join(format!("{id}.tar"))
}

/// Move each archive a queue written before archives had files of their own
/// still holds into its file (or drop it, its job having ended), and say
/// whether there was any.
fn move_archives(archives: &Path, jobs: &mut [Entry]) -> Result<bool> {
    let mut moved = false;
    for entry in jobs {
        if entry.archive.is_empty() {
            continue;
        }
        if !entry.job.state.is_finished() {
            let bytes = shared::base64::decode(&entry.archive)
                .map_err(|error| anyhow!("job {}'s archive in the queue: {error}", entry.job.id))?;
            let file = archive_file(archives, &entry.job.id);
            write_atomically(&file, &bytes).with_context(|| {
                format!("moving job {}'s archive out of the queue", entry.job.id)
            })?;
        }
        entry.archive.clear();
        moved = true;
    }
    Ok(moved)
}

/// Remove every archive kept for a job that has ended or been forgotten: an
/// archive is kept exactly as long as its job is unfinished. Says the ids of
/// the jobs whose archives were removed.
fn sweep_archives(archives: &Path, jobs: &[Entry]) -> Vec<String> {
    let Ok(found) = std::fs::read_dir(archives) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for file in found.flatten() {
        let path = file.path();
        let kept = jobs.iter().any(|entry| {
            !entry.job.state.is_finished() && path == archive_file(archives, &entry.job.id)
        });
        if !kept
            && std::fs::remove_file(&path).is_ok()
            && let Some(id) = path.file_stem().and_then(|stem| stem.to_str())
        {
            removed.push(id.to_string());
        }
    }
    removed
}

/// How often a chain waiting to be taken on is looked at again: whether the
/// key is blessed there and holds gas.
const BLESSING_LOOKED_AT_EVERY: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(10)
};

/// Take a chain on once it is connected: get the key blessed there, pick the
/// jobs on it a restart caught mid-way back up, and only then take its
/// uploads and build its jobs, and read its payments on from where the last
/// run left off. A step that fails is tried again, waiting longer each time.
async fn take_on(service: Arc<Service>, on: Arc<Served>) {
    let chain = on.vault_chain;
    match (&service.attestation, &service.key) {
        // In the attested image the key is blessed by the launcher's token,
        // and nothing on a chain is taken until it is, and until the key
        // holds gas for a registration.
        (Some(attestation), Some(key)) => {
            attestation.serve_chain(on.client.clone(), on.deployments.clone());
            let mut said = false;
            loop {
                let ready = attestation.ready(on.client.chain_id);
                if ready
                    && cloud::holds_gas(&on.client, key.address())
                        .await
                        .unwrap_or(false)
                {
                    break;
                }
                if !said {
                    tracing::info!(%chain, blessed = ready, "waiting for the key's blessing and gas");
                    said = true;
                }
                tokio::time::sleep(BLESSING_LOOKED_AT_EVERY).await;
            }
        }
        _ => {
            until_done(chain, "the key could not be blessed", || {
                bless(&on.client, &on.deployments)
            })
            .await
        }
    }
    until_done(
        chain,
        "the jobs a restart caught mid-way could not be picked up",
        || pick_up(&service, &on),
    )
    .await;
    if let Ok(mut connected) = service.connected.write() {
        connected.insert(chain, on.clone());
    }
    tracing::info!(%chain, "taking uploads and building jobs on this chain");

    let mut from = service
        .payments_read
        .lock()
        .await
        .get(&chain.to_string())
        .copied()
        .unwrap_or(on.deployments.deployed_block);
    // Where payments were last noted read through, for the wake plan.
    let mut noted = None;
    let chain_id = on.client.chain_id.to_string();
    let mut wait = STALLED_FIRST_WAIT;
    loop {
        let Some(turn) = service.turns.turn().await else {
            return;
        };
        let read = admit(&service, &on, &mut from).await;
        // Read through further, or not yet noted: one past read a turn at most.
        if noted != Some(from) && note_payments_seen(&service, &on, from).await {
            noted = Some(from);
        }
        drop(turn);
        let Err(error) = read else {
            if service
                .payments_stalled
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&chain_id)
                .is_some()
            {
                tracing::info!(%chain, "payments on this chain are read again");
            }
            wait = STALLED_FIRST_WAIT;
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };
        let halted = error.downcast_ref::<Halted>();
        service
            .payments_stalled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                chain_id.clone(),
                Stalled {
                    retry_at: unix_now() + wait.as_secs(),
                    paused: halted.is_some(),
                },
            );
        match halted {
            Some(halted) => tracing::warn!(
                %chain,
                from = halted.from,
                to = halted.to,
                error = halted.why,
                seconds = wait.as_secs(),
                "payments on this chain wait at these blocks: their logs could not be read, and \
                 the registry's record of them does not prove which payments they hold; \
                 read again later"
            ),
            None => tracing::warn!(
                %chain,
                error = format!("{error:#}"),
                seconds = wait.as_secs(),
                "could not read payments; trying again"
            ),
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(STALLED_LONGEST_WAIT);
    }
}

/// A chain whose last turn of reading payments failed: it is read again at
/// `retry_at` (unix seconds), sooner after a first failure, later after
/// each next one, never later than [`STALLED_LONGEST_WAIT`]. While a payment
/// may wait unread there, the chain gives the wake plan that time, so the
/// waker starts the build service again by then if it has exited.
#[derive(Clone, Copy, Debug)]
struct Stalled {
    retry_at: u64,
    /// Whether the registry's record could not prove which payments blocks
    /// whose logs could not be read hold ([`Halted`]), rather than a read
    /// that failed: that is not waited for with the build service kept up.
    paused: bool,
}

/// The wait before reading payments again on a chain whose last turn
/// failed; it doubles with each failed turn after.
const STALLED_FIRST_WAIT: Duration = Duration::from_secs(5);

/// The longest wait before reading payments again on a chain whose turns
/// keep failing: the most a payment there waits for the build service to
/// try again, once the waker honours the plan's retry time.
const STALLED_LONGEST_WAIT: Duration = Duration::from_secs(15 * 60);

/// How soon a failed read must be tried again for a payment that may wait
/// behind it to keep the build service up: a brief failure is read past in
/// the same run, a longer one is left to the plan's retry time.
const STALLED_KEEPS_UP: Duration = Duration::from_secs(60);

/// Note how many payments the registry on `on` had taken at the last block
/// payments were read through (`from` is the next to read), for the wake
/// plan: a payment past that wakes the build service. Says whether it was
/// noted.
async fn note_payments_seen(service: &Service, on: &Served, from: u64) -> bool {
    let Some(through) = from.checked_sub(1) else {
        return false;
    };
    match on
        .client
        .read_at(
            on.deployments.publishing,
            IPublishing::countCall {},
            alloy::eips::BlockId::number(through),
        )
        .await
    {
        Ok(count) => {
            service
                .payments_seen
                .lock()
                .await
                .insert(on.client.chain_id.to_string(), count.saturating_to());
            true
        }
        Err(error) => {
            tracing::debug!(chain = %on.vault_chain, error = format!("{error:#}"), "the payments taken could not be counted");
            false
        }
    }
}

/// Do `step` until it is done, saying why not at warn each time, and waiting
/// five seconds, doubling, never more than five minutes, between tries.
async fn until_done<F, Fut>(chain: Chain, failed: &str, step: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let mut wait = Duration::from_secs(5);
    while let Err(error) = step().await {
        tracing::warn!(%chain, error = format!("{error:#}"), seconds = wait.as_secs(), "{failed}; trying again");
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(Duration::from_secs(300));
    }
}

/// Pick the jobs on `on` a restart caught mid-way back up (see [`picked_up`]).
async fn pick_up(service: &Service, on: &Served) -> Result<()> {
    // The chain's jobs caught mid-way are never taken from the queue until it
    // is connected, so nothing changes them while the registry is read.
    let jobs = service.queue.lock().await.jobs.clone();
    let registered = registered_before_the_restart(on, &jobs).await?;
    let mut queue = service.queue.lock().await;
    picked_up(
        &mut queue.jobs,
        on.vault_chain,
        &registered,
        |bundle_hash| service.holds(bundle_hash),
    );
    let count = queue
        .jobs
        .iter()
        .filter(|entry| entry.job.chain == on.vault_chain && !entry.job.state.is_finished())
        .count();
    if count > 0 {
        tracing::info!(chain = %on.vault_chain, count, "picked the queue back up");
    }
    service.sweep(&queue.jobs);
    write_queue(service, &mut queue).await
}

/// Get this image's key blessed, so the registry will accept what it signs.
async fn bless(chain: &Client, deployments: &Deployments) -> Result<()> {
    let key = deployments.roles.builder;
    chain::attest::bless_service(chain, deployments, "builder", key).await?;
    chain::attest::keep_blessed(
        chain.clone(),
        deployments.clone(),
        "builder",
        key,
        chain::attest::Sender::Unlocked(key),
        Address::ZERO,
        B256::ZERO,
        || {},
    );
    tracing::info!(key = %key, "the build service is blessed");
    Ok(())
}

// Uploads and payments --------------------------------------------------------

type Refusal = (StatusCode, String);

fn bad(message: impl Into<String>) -> Refusal {
    (StatusCode::BAD_REQUEST, message.into())
}

/// Each upload's own scratch directory, so two at once never share one.
static UPLOADS: AtomicU64 = AtomicU64::new(0);

/// Take a project, check what can be checked before anyone pays, and hand
/// back the reference to pay against. A manifest the platform won't accept,
/// or source over the limit, is refused here, before the $1 is spent.
async fn upload(
    State(service): State<Arc<Service>>,
    UrlPath(chain): UrlPath<Chain>,
    upload: Result<Json<Upload>, JsonRejection>,
) -> Result<Json<UploadAccepted>, Refusal> {
    let Json(upload) = upload.map_err(|rejection| {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            (rejection.status(), shared::archive::upload_too_large())
        } else {
            (rejection.status(), rejection.body_text())
        }
    })?;
    if !service.configured.contains(&chain) {
        return Err(bad(format!("this build service does not serve {chain}")));
    }
    // Neither the upload nor its job could be kept yet: asked again, it is
    // taken once they can.
    if !service.keeps_state() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "the build service is starting and takes uploads once its key is blessed: try \
             again shortly"
                .to_string(),
        ));
    }
    if !service.connected_to(chain) {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("{chain}'s node is not answering yet: try again shortly"),
        ));
    }
    // Sealed source must reach the attested build service sealed to it, and
    // that path is not built yet: one sent in the clear to it is refused,
    // rather than taken as sealed.
    if upload.sealed && service.key.is_some() {
        return Err(bad(
            "a sealed upload is refused in the cloud until the sealed path is built",
        ));
    }
    let author = Wallet::on(chain, &upload.author).map_err(bad)?;
    let archive = shared::base64::decode(&upload.archive_base64).map_err(bad)?;
    let source_hash = hash_bytes(&archive);

    // Read it the way the build will, in a directory of its own: this
    // request's, never another's sent at the same time.
    let scratch = service.builds.join(format!(
        "upload-{}-{}",
        &source_hash[7..23],
        UPLOADS.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    let checked = shared::archive::unpack(&archive, &scratch)
        .map_err(|error| format!("the archive: {error}"))
        .and_then(|()| check(&service, &scratch, upload.package.as_deref(), chain))
        // The reviewer reads every file, whatever it is named, since code can
        // be in any of them: a project it could not read whole, a file that
        // is not text or more than it reads in one review, is refused here,
        // before it is paid for.
        .and_then(|checked| {
            shared::archive::reviewed(&archive)
                .map(|_| checked)
                .map_err(|error| error.to_string())
        })
        // Nor is anything built but what the reviewer reads and the crates
        // the platform reviewed itself: a project depending on any other
        // source is refused here too.
        .and_then(|checked| {
            shared::dependencies::checked(&archive, &checked.1, &service.allowed).map(|_| checked)
        });
    let _ = std::fs::remove_dir_all(&scratch);
    let (manifest, manifest_path, package) = checked.map_err(bad)?;

    let mut queue = service.queue.lock().await;
    let read_to = service.payments_read_to.lock().await.clone();
    if forget_unpaid(&mut queue, &read_to, &service.served(), now()) {
        service.sweep(&queue.jobs);
        let _ = write_queue(&service, &mut queue).await;
    }
    // An upload is kept by its archive's file, made before the queue names
    // it, and by the queue naming it, written down before it is answered;
    // one that can't be kept is turned away rather than taken and lost.
    let before = (
        queue.jobs.clone(),
        queue.forgotten.clone(),
        queue.credits.clone(),
    );
    let keep = |id: String| {
        let service = service.clone();
        let archive = archive.clone();
        async move {
            service
                .keep(
                    &archive_file(&service.archives, &id),
                    &archive_name(&id),
                    &archive,
                )
                .await
                .map_err(|error| {
                    tracing::warn!(
                        error = format!("{error:#}"),
                        "an upload's archive could not be kept"
                    );
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "the upload could not be kept: try again later".to_string(),
                    )
                })
        }
    };
    // The same project sent again by the same author after it was forgotten,
    // or after another payment for it, and paid for since, is admitted on
    // that payment: it is not paid twice.
    if let Some(at) = queue.credits.iter().position(|credit| {
        credit.paid_for.is(
            chain,
            author,
            &source_hash,
            upload.sealed,
            &manifest_path,
            &package,
        )
    }) {
        let id = queue.next_id.to_string();
        queue.next_id += 1;
        keep(id.clone()).await?;
        let credit = queue.credits.remove(at);
        let reference = credit.paid_for.upload;
        let mut job = new_job(&id, chain, author, &manifest, source_hash, upload.sealed);
        job.strategy_id = Some(credit.strategy_id);
        job.state = JobState::Queued { position: 0 };
        queue.jobs.push(Entry {
            job,
            upload: reference,
            author,
            archive: String::new(),
            manifest_path,
            package,
            attempts: 0,
            reviewer_away: 0,
            not_before: None,
        });
        written_down(&service, &mut queue, &id, before).await?;
        tracing::info!(
            id,
            strategy_id = credit.strategy_id,
            "an upload paid for already was sent again, and is queued"
        );
        return Ok(Json(UploadAccepted {
            upload: format!("{reference:#x}"),
            job_id: id,
        }));
    }
    // The same project sent again by the same author before it is paid for
    // (a retry after an answer that never arrived, say) is the upload already
    // waiting, not a second job: there is nothing to pay for twice.
    if let Some(waiting) = waiting_for_payment(
        &queue.jobs,
        chain,
        author,
        &source_hash,
        upload.sealed,
        &manifest_path,
        &package,
    ) {
        tracing::info!(
            id = waiting.job.id,
            "an upload already waiting was sent again"
        );
        return Ok(Json(UploadAccepted {
            upload: format!("{:#x}", waiting.upload),
            job_id: waiting.job.id.clone(),
        }));
    }
    let id = queue.next_id.to_string();
    queue.next_id += 1;
    keep(id.clone()).await?;
    // Unique to this author, this source and this job, so a second upload of
    // the same project once the first is paid for is a second job with its
    // own reference.
    let reference = keccak256(
        [
            author.bytes().as_slice(),
            &hash_to_bytes(&source_hash).unwrap_or_default(),
            id.as_bytes(),
        ]
        .concat(),
    );
    let job = new_job(&id, chain, author, &manifest, source_hash, upload.sealed);
    queue.jobs.push(Entry {
        job,
        upload: reference,
        author,
        archive: String::new(),
        manifest_path,
        package,
        attempts: 0,
        reviewer_away: 0,
        not_before: None,
    });
    let forgot = bound_unpaid(&mut queue, author, now());
    written_down(&service, &mut queue, &id, before).await?;
    if forgot {
        service.sweep(&queue.jobs);
    }
    tracing::info!(id, "an upload is waiting for its payment");
    Ok(Json(UploadAccepted {
        upload: format!("{reference:#x}"),
        job_id: id,
    }))
}

/// Write down the queue now naming upload `id`. If it can't be, the queue in
/// memory is put back as it was `before` the upload and its archive removed,
/// and the upload is turned away: answered as taken, it would be lost at the
/// next restart, and the payment made against it with it. The author can
/// send it again.
async fn written_down(
    service: &Service,
    queue: &mut Queue,
    id: &str,
    before: (Vec<Entry>, Vec<Forgotten>, Vec<Credit>),
) -> Result<(), Refusal> {
    if write_queue(service, queue).await.is_ok() {
        return Ok(());
    }
    (queue.jobs, queue.forgotten, queue.credits) = before;
    let _ = std::fs::remove_file(archive_file(&service.archives, id));
    service.let_go(vec![id.to_string()]);
    Err((
        StatusCode::SERVICE_UNAVAILABLE,
        "the upload could not be kept: try again later".to_string(),
    ))
}

/// A job just uploaded, waiting for its payment.
fn new_job(
    id: &str,
    chain: Chain,
    author: Wallet,
    manifest: &Manifest,
    source_hash: String,
    sealed: bool,
) -> Job {
    Job {
        id: id.to_string(),
        chain,
        strategy_id: None,
        title: manifest.description.title.clone(),
        author: author.to_string(),
        state: JobState::AwaitingPayment,
        submitted: now(),
        duration_seconds: 0,
        source_hash,
        sealed,
        bundle_hash: None,
        review: None,
        strategy_slug: None,
    }
}

/// Whether `since` is at least `long` ago, both by the clock and in the chain
/// time `read_to` says its payments have been read through. A chain whose
/// payments have not been read yet is never past anything.
fn long_past(
    since: time::PrimitiveDateTime,
    long: time::Duration,
    read_to: Option<&time::PrimitiveDateTime>,
    now: time::PrimitiveDateTime,
) -> bool {
    now - since >= long && read_to.is_some_and(|&read_to| read_to - since >= long)
}

/// Forget every upload that has waited [`UNPAID_FOR`] for its payment,
/// archive and all, remembering only enough to credit a payment that comes
/// late; and stop remembering those forgotten [`FORGOTTEN_FOR`] ago. Both are
/// measured against the payments read as well as the clock: while payments
/// on a chain are behind, nothing on it is forgotten. On a chain no longer
/// `served`, whose payments are not being read at all, an upload is
/// forgotten, and what is remembered of it then let go, by the clock alone;
/// until then, what is remembered still credits its payment should the chain
/// be served again. Says whether anything changed.
fn forget_unpaid(
    queue: &mut Queue,
    read_to: &BTreeMap<Chain, time::PrimitiveDateTime>,
    served: &[Chain],
    now: time::PrimitiveDateTime,
) -> bool {
    let before = (queue.jobs.len(), queue.forgotten.len());
    let mut forgotten = Vec::new();
    queue.jobs.retain(|entry| {
        let chain = entry.job.chain;
        let expired = entry.job.state == JobState::AwaitingPayment
            && if served.contains(&chain) {
                long_past(entry.job.submitted, UNPAID_FOR, read_to.get(&chain), now)
            } else {
                now - entry.job.submitted >= UNPAID_FOR
            };
        if expired {
            tracing::info!(id = entry.job.id, "an upload never paid for is forgotten");
            forgotten.push(Forgotten::of(entry, now));
        }
        !expired
    });
    queue.forgotten.retain(|old| {
        let kept = !if served.contains(&old.chain) {
            long_past(old.at, FORGOTTEN_FOR, read_to.get(&old.chain), now)
        } else {
            now - old.at >= FORGOTTEN_FOR
        };
        if !kept {
            tracing::info!(upload = %old.upload, "a forgotten upload is no longer remembered");
        }
        kept
    });
    queue.forgotten.extend(forgotten);
    (queue.jobs.len(), queue.forgotten.len()) != before
}

/// Hold the uploads waiting for their payment to the bounds: past
/// [`UNPAID_PER_WALLET`] naming `author`, or [`UNPAID_AT_ALL`] from everyone,
/// the oldest waiting under that bound is forgotten, as one forgotten for its
/// age is, so a payment for it still becomes a credit. Nobody proves they own
/// the wallet an upload names, so turning a new upload away instead would let
/// anyone lock a wallet, or everyone, out of publishing. Says whether
/// anything was forgotten.
fn bound_unpaid(queue: &mut Queue, author: Wallet, now: time::PrimitiveDateTime) -> bool {
    let waiting = |entry: &Entry| entry.job.state == JobState::AwaitingPayment;
    let theirs = |entry: &Entry| waiting(entry) && entry.author == author;
    let mut forgot = false;
    for (bound, under) in [
        (UNPAID_PER_WALLET, &theirs as &dyn Fn(&Entry) -> bool),
        (UNPAID_AT_ALL, &waiting),
    ] {
        while queue.jobs.iter().filter(|entry| under(entry)).count() > bound {
            let Some(oldest) = queue.jobs.iter().position(under) else {
                break;
            };
            let entry = queue.jobs.remove(oldest);
            tracing::info!(
                id = entry.job.id,
                "an upload waiting for its payment is forgotten to make room for a newer one"
            );
            queue.forgotten.push(Forgotten::of(&entry, now));
            forgot = true;
        }
    }
    forgot
}

/// The upload still waiting for its payment that is this one again: the same
/// author, chain and source, built the same way.
fn waiting_for_payment<'a>(
    jobs: &'a [Entry],
    chain: Chain,
    author: Wallet,
    source_hash: &str,
    sealed: bool,
    manifest_path: &str,
    package: &str,
) -> Option<&'a Entry> {
    jobs.iter().find(|entry| {
        entry.job.state == JobState::AwaitingPayment
            && entry.job.chain == chain
            && entry.author == author
            && entry.job.source_hash == source_hash
            && entry.job.sealed == sealed
            && entry.manifest_path == manifest_path
            && entry.package == package
    })
}

/// Everything about a project that can be refused before it is paid for.
fn check(
    service: &Service,
    source: &Path,
    package: Option<&str>,
    chain: Chain,
) -> Result<(Manifest, String, String), String> {
    if build::source_size(source) > build::SIZE_LIMIT {
        return Err(Failure::TooLarge.label().into());
    }
    let path = build::find_manifest(source, package)?;
    if let Some(refused) = build::overrides(source) {
        return Err(refused);
    }
    let manifest = build::read_manifest(&path)?;
    manifest.check(&service.catalogue, &service.network)?;
    if manifest.vault_chain()? != chain {
        return Err(format!(
            "shonin.toml puts the vault on {}, and this upload is for {chain}",
            manifest.vault.chain
        ));
    }
    let package = build::package_of(&path)?;
    let relative = path
        .strip_prefix(source)
        .map_err(|_| "the manifest is outside the project")?
        .to_string_lossy()
        .to_string();
    Ok((manifest, relative, package))
}

/// Read the payments on one chain from `from` towards the confirmed head
/// (the head, less the chain's confirmations), at most [`PAGES_PER_TURN`]
/// pages a turn, and admit the jobs they pay for. As many blocks as the
/// chain's confirmations behind `from` are read again each time, so a
/// payment a node showed late is still read; one already acted on is
/// skipped. `from` is moved on, and kept, only once what a page admitted is
/// written down: a payment is never read past while its admission could
/// still be lost. A payment starts a job only if it came from the wallet the
/// upload named, so a copied payment from anyone else starts nothing.
///
/// More than a page behind, the blocks before the next payment are passed
/// over unread when the registry proves they hold none ([`first_payable`]).
/// A page whose logs still fail after [`PAGE_TRIES`] tries is passed over
/// only with every payment in it taken from the registry's record
/// ([`recorded_between`]). When that record can't be read, the turn fails
/// and is tried again; when it is read but doesn't prove which payments the
/// page holds, the read ends in [`Halted`]. Neither passes over a payment.
async fn admit(service: &Arc<Service>, on: &Served, from: &mut u64) -> Result<()> {
    let confirmations = service
        .confirmations
        .get(&on.vault_chain)
        .copied()
        .unwrap_or(CONFIRMATIONS);
    let Some(confirmed) = on.client.block_number().await?.checked_sub(confirmations) else {
        return Ok(());
    };
    let mut at = from
        .saturating_sub(confirmations)
        .max(on.deployments.deployed_block);
    if confirmed.saturating_sub(at) >= PAYMENT_SPAN {
        match first_payable(on, at, confirmed).await {
            Ok(first) if first > at => {
                tracing::info!(
                    chain = %on.vault_chain,
                    from = at,
                    to = first - 1,
                    "passed over blocks the registry shows hold no payment"
                );
                at = first;
                if at > *from {
                    read_to(service, on, from, at).await;
                }
            }
            Ok(_) => {}
            Err(error) => tracing::debug!(
                chain = %on.vault_chain,
                error = format!("{error:#}"),
                "the registry could not say where the next payment is: payments are read page by page"
            ),
        }
    }
    let mut pages = 0;
    while at <= confirmed && pages < PAGES_PER_TURN {
        pages += 1;
        let mut to = confirmed.min(at + PAYMENT_SPAN - 1);
        let payments = match read_page(on, at, to).await {
            Ok(payments) => payments,
            Err(error) => {
                tracing::warn!(
                    chain = %on.vault_chain,
                    from = at,
                    to,
                    tries = PAGE_TRIES,
                    error = format!("{error:#}"),
                    "the payments in these blocks could not be read: taking them from the registry's record"
                );
                match recorded_between(on, at, to, confirmed)
                    .await
                    .with_context(|| {
                        format!(
                            "the payments in blocks {at} to {to} could not be read, \
                             from their logs nor from the registry's record"
                        )
                    })? {
                    Recorded::Proven { payments, through } => {
                        tracing::warn!(
                            chain = %on.vault_chain,
                            from = at,
                            to = through,
                            payments = payments.len(),
                            "passed over blocks whose logs could not be read, with every \
                             payment the registry records in them"
                        );
                        to = through;
                        payments
                    }
                    Recorded::Unproven(why) => {
                        return Err(Halted { from: at, to, why }.into());
                    }
                }
            }
        };
        if !payments.is_empty() {
            admit_payments(service, on.vault_chain, payments).await?;
        }
        at = to + 1;
        if at > *from {
            read_to(service, on, from, at).await;
        }
    }
    // How far payments were read, in the chain's own time: for a restart, too,
    // where it is known in blocks only.
    if let Some(through) = from.checked_sub(1)
        && through >= on.deployments.deployed_block
        && let Err(error) = read_through(service, on, through).await
    {
        tracing::debug!(
            chain = %on.vault_chain,
            error = format!("{error:#}"),
            "the time of the last block payments were read through could not be read"
        );
    }
    Ok(())
}

/// The most pages of payments one turn reads: a chain far behind is caught
/// up over several turns, and an idle exit waits for one turn at most.
const PAGES_PER_TURN: u32 = 25;

/// How many times a page of payments is asked for before it is passed over.
const PAGE_TRIES: u32 = 3;

/// The wait before a page of payments is asked for again, doubled each time.
const PAGE_RETRY_WAIT: Duration = Duration::from_secs(2);

/// Payments on a chain could not be read in blocks `from..=to`, and the
/// registry's record, read, does not prove which payments those blocks
/// hold: reading on could pass over a payment, so that chain's payments
/// wait there ([`Stalled`]), without keeping the build service up.
#[derive(Debug)]
struct Halted {
    from: u64,
    to: u64,
    why: String,
}

impl std::fmt::Display for Halted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "payments in blocks {} to {} could not be read: {}",
            self.from, self.to, self.why
        )
    }
}

impl std::error::Error for Halted {}

/// Move where payments were read to on to `at`, and keep it.
async fn read_to(service: &Service, on: &Served, from: &mut u64, at: u64) {
    *from = at;
    let mut read = service.payments_read.lock().await;
    read.insert(on.vault_chain.to_string(), at);
    match serde_json::to_vec(&*read) {
        Ok(bytes) => {
            if let Err(error) = service.keep(&service.payments_file, PAYMENTS, &bytes).await {
                tracing::warn!(
                    error = format!("{error:#}"),
                    "where payments were read to could not be written down"
                );
            }
        }
        Err(error) => {
            tracing::warn!(%error, "where payments were read to could not be written down")
        }
    }
}

/// The payments in blocks `from..=to`, from their logs, asked for up to
/// [`PAGE_TRIES`] times. The client reads the range in windows each
/// endpoint takes, and moves past an endpoint that is behind, limits the
/// rate or refuses.
async fn read_page(on: &Served, from: u64, to: u64) -> Result<Vec<Payment>> {
    let filter = Filter::new()
        .address(on.deployments.publishing)
        .event_signature(IPublishing::Submitted::SIGNATURE_HASH)
        .from_block(from)
        .to_block(to);
    let mut wait = PAGE_RETRY_WAIT;
    let mut tries = 1;
    loop {
        match on.client.logs(&filter).await {
            Ok(logs) => return paid(logs),
            Err(error) if tries < PAGE_TRIES => {
                tracing::debug!(
                    chain = %on.vault_chain,
                    from,
                    to,
                    error = format!("{error:#}"),
                    "the payments in these blocks could not be read; asking again"
                );
                tokio::time::sleep(wait).await;
                wait *= 2;
                tries += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// How many payments the registry on `on` had taken by the end of `block`:
/// none before it was deployed. Read only through an endpoint known to
/// answer as of the block asked, never one that may answer with an earlier
/// or later block's state: blocks are passed over on this count.
async fn count_at(on: &Served, block: Option<u64>) -> Result<u64> {
    let Some(block) = block.filter(|block| *block >= on.deployments.deployed_block) else {
        return Ok(0);
    };
    let count = on
        .client
        .read_at_honoured(on.deployments.publishing, IPublishing::countCall {}, block)
        .await?;
    Ok(count.saturating_to())
}

/// The registry's record of payment `id`: who paid, for what, and when. A
/// record not there (an endpoint whose state is behind answers so for a
/// payment it hasn't seen) is an error, never a payment made at time 0.
async fn submission(on: &Served, id: u64) -> Result<chain::bindings::publishing::Submission> {
    let paid = on
        .client
        .read(
            on.deployments.publishing,
            IPublishing::submissionOfCall {
                strategyId: alloy::primitives::U256::from(id),
            },
        )
        .await?;
    if paid.author == Address::ZERO || paid.paidAt == 0 {
        bail!("the registry's record of payment {id} is not there to be read");
    }
    Ok(paid)
}

/// Whether payment `id` was made before block `block`, by its own record:
/// paid before that block's time. Payment 0 is none, made before any block.
async fn paid_before(on: &Served, id: u64, block: u64) -> Result<bool> {
    Ok(id == 0 || submission(on, id).await?.paidAt < on.client.block_timestamp_at(block).await?)
}

/// Whether payment `id` was made after block `block`, by its own record:
/// paid after that block's time, or not made by block `last` (`taken` is the
/// count there, `block <= last`).
async fn paid_after(on: &Served, id: u64, block: u64, taken: u64) -> Result<bool> {
    Ok(
        id > taken
            || submission(on, id).await?.paidAt > on.client.block_timestamp_at(block).await?,
    )
}

/// How many block edges a proof by the times payments were made moves
/// past, one block at a time, when an edge falls between two blocks of the
/// same time (which can't tell a payment in one from a payment in the
/// other).
const EDGE_SHIFTS: u64 = 4;

/// The last block from `from` to `to`, `to` first then each block before it
/// down to [`EDGE_SHIFTS`] blocks, such that no payment was made in blocks
/// `from..=it`, `before` being the count before `from`; `None` when none of
/// those is proven. Ids follow the blocks they were paid in, so it is
/// proven by the times the payments on either side were made: payment
/// `before` before `from`'s time, and payment `before + 1` after the last
/// block's. `taken` is the count at a block at or past `to`.
async fn none_through(
    on: &Served,
    from: u64,
    to: u64,
    before: u64,
    taken: u64,
) -> Result<Option<u64>> {
    if before > taken || !paid_before(on, before, from).await? {
        return Ok(None);
    }
    for last in (from.max(to.saturating_sub(EDGE_SHIFTS))..=to).rev() {
        if paid_after(on, before + 1, last, taken).await? {
            return Ok(Some(last));
        }
    }
    Ok(None)
}

/// The first block from `from` to `to` that may hold a payment, or `to + 1`
/// if none does, as far as the registry proves it: `from` when it proves
/// nothing. The counts at past blocks find the block where the count first
/// rises, and [`none_through`] proves the blocks before it hold none. Every
/// count is read as of the block asked ([`count_at`]), so an endpoint behind
/// fails it rather than pass it.
async fn first_payable(on: &Served, from: u64, to: u64) -> Result<u64> {
    let before = count_at(on, from.checked_sub(1)).await?;
    let taken = count_at(on, Some(to)).await?;
    let mut first = to + 1;
    if taken != before {
        let (mut low, mut high) = (from, to);
        while low < high {
            let middle = low + (high - low) / 2;
            if count_at(on, Some(middle)).await? > before {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        first = low;
    }
    if first == from {
        return Ok(from);
    }
    Ok(none_through(on, from, first - 1, before, taken)
        .await?
        .map_or(from, |last| last + 1))
}

/// What the registry's record says of the payments in a span of blocks.
enum Recorded {
    /// Every payment made in blocks from the span's first to `through`.
    Proven {
        payments: Vec<Payment>,
        through: u64,
    },
    /// The record was read, but doesn't prove which payments the span holds.
    Unproven(String),
}

/// Every payment made in blocks `from..=to`, or in a span from `from` that
/// ends a few blocks short of `to` ([`EDGE_SHIFTS`]), from the registry's
/// record rather than its logs: the payments numbered after the count
/// before `from` up to the count at the span's end, once the times payments
/// were made prove none before or after them falls in those blocks
/// ([`none_through`], with the first block moved back as far when its edge
/// is what fails). An error when the record can't be read (it may say more
/// when read again); [`Recorded::Unproven`] when it is read and proves
/// nothing.
async fn recorded_between(on: &Served, from: u64, to: u64, confirmed: u64) -> Result<Recorded> {
    let taken = count_at(on, Some(confirmed)).await?;
    let mut start = None;
    let lowest = from
        .saturating_sub(EDGE_SHIFTS)
        .max(on.deployments.deployed_block);
    for first in (lowest..=from).rev() {
        let before = count_at(on, first.checked_sub(1)).await?;
        if before <= taken && paid_before(on, before, first).await? {
            start = Some(before);
            break;
        }
    }
    let Some(before) = start else {
        return Ok(Recorded::Unproven(
            "the payments before these blocks were not all made before their first block's \
             time"
                .into(),
        ));
    };
    for last in (from.max(to.saturating_sub(EDGE_SHIFTS))..=to).rev() {
        let through = count_at(on, Some(last)).await?;
        if through < before || through > taken {
            return Ok(Recorded::Unproven(format!(
                "the registry's count of payments goes from {before} to {through} through these \
                 blocks, and stands at {taken} later"
            )));
        }
        if !paid_after(on, through + 1, last, taken).await? {
            continue;
        }
        let mut payments = Vec::new();
        for id in before + 1..=through {
            let paid = submission(on, id).await?;
            payments.push(Payment {
                strategy_id: id,
                upload: paid.upload,
                author: Wallet::Evm(paid.author),
            });
        }
        return Ok(Recorded::Proven {
            payments,
            through: last,
        });
    }
    Ok(Recorded::Unproven(
        "the payment after these blocks was not made after their last block's time".into(),
    ))
}

/// Note that every payment on `on` up to `block` has been read and acted on,
/// in the chain's own time: only then may an upload waiting there be
/// forgotten.
async fn read_through(service: &Service, on: &Served, block: u64) -> Result<()> {
    let seconds = on.client.block_timestamp_at(block).await?;
    let at = time::OffsetDateTime::from_unix_timestamp(seconds.try_into()?)?;
    service.payments_read_to.lock().await.insert(
        on.vault_chain,
        time::PrimitiveDateTime::new(at.date(), at.time()),
    );
    Ok(())
}

/// The payments these logs record.
fn paid(logs: Vec<alloy::rpc::types::Log>) -> Result<Vec<Payment>> {
    let mut payments = Vec::new();
    for log in logs {
        let Ok(paid) = log.log_decode::<IPublishing::Submitted>() else {
            continue;
        };
        let paid = paid.inner.data;
        let id: u64 = paid
            .strategyId
            .try_into()
            .context("a strategy id past u64")?;
        payments.push(Payment {
            strategy_id: id,
            upload: paid.upload,
            author: Wallet::Evm(paid.author),
        });
    }
    Ok(payments)
}

/// One payment to publish, as a chain records it: the id it took, the
/// upload it names and the wallet it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Payment {
    strategy_id: u64,
    upload: B256,
    author: Wallet,
}

/// Queue the jobs these payments on `chain` pay for (see [`admit`]).
async fn admit_payments(
    service: &Arc<Service>,
    chain: Chain,
    payments: Vec<Payment>,
) -> Result<()> {
    let mut queue = service.queue.lock().await;
    let mut changed = false;
    for paid in payments {
        let id = paid.strategy_id;
        let names = |upload: B256, author: Wallet, on: Chain| {
            upload == paid.upload && author == paid.author && on == chain
        };
        if let Some(entry) = queue.jobs.iter_mut().find(|entry| {
            names(entry.upload, entry.author, entry.job.chain)
                && entry.job.state == JobState::AwaitingPayment
        }) {
            entry.job.strategy_id = Some(id);
            entry.job.state = JobState::Queued { position: 0 };
            tracing::info!(job = entry.job.id, id, "paid for, and queued");
            changed = true;
            continue;
        }
        // A payment read before (pages are read again) has done all it will
        // do.
        let already = queue
            .jobs
            .iter()
            .any(|entry| entry.job.chain == chain && entry.job.strategy_id == Some(id))
            || queue
                .credits
                .iter()
                .any(|credit| credit.paid_for.chain == chain && credit.strategy_id == id);
        if already {
            continue;
        }
        // One for an upload forgotten, or for one another payment admitted
        // first, admits the same upload when it is sent again.
        let paid_for = queue
            .forgotten
            .iter()
            .find(|forgotten| names(forgotten.upload, forgotten.author, forgotten.chain))
            .cloned()
            .or_else(|| {
                queue
                    .jobs
                    .iter()
                    .find(|entry| names(entry.upload, entry.author, entry.job.chain))
                    .map(|entry| Forgotten::of(entry, now()))
            });
        if let Some(paid_for) = paid_for {
            // The same upload, sent again by the same author before this
            // payment was read, is admitted on it now: it is not paid twice.
            // The reference it was given to pay against is forgotten, so a
            // payment made against that one too becomes a credit.
            if let Some(again) = queue.jobs.iter_mut().find(|entry| {
                entry.job.state == JobState::AwaitingPayment
                    && paid_for.is(
                        entry.job.chain,
                        entry.author,
                        &entry.job.source_hash,
                        entry.job.sealed,
                        &entry.manifest_path,
                        &entry.package,
                    )
            }) {
                let unneeded = Forgotten::of(again, now());
                again.upload = paid_for.upload;
                again.job.strategy_id = Some(id);
                again.job.state = JobState::Queued { position: 0 };
                tracing::info!(
                    job = again.job.id,
                    id,
                    "an upload forgotten, or paid for already, was paid for after it was sent \
                     again: queued on this payment"
                );
                queue.forgotten.push(unneeded);
                changed = true;
                continue;
            }
            tracing::info!(
                id,
                upload = %paid.upload,
                "an upload forgotten, or paid for already, was paid for: sent again, it is \
                 admitted on this payment"
            );
            queue.credits.push(Credit {
                paid_for,
                strategy_id: id,
            });
            changed = true;
            continue;
        }
        if queue.unmatched.insert((chain, id)) {
            tracing::warn!(
                id,
                upload = %paid.upload,
                author = %paid.author,
                "a payment names no upload waiting from that wallet, nor one forgotten in the \
                 last week, so it admits nothing"
            );
        }
    }
    if changed || queue.unwritten {
        write_queue(service, &mut queue)
            .await
            .context("what payments admitted could not be written down, so they are read again")?;
    }
    Ok(())
}

/// The public queue. An upload not yet paid for is anyone's, naming any
/// wallet, so it is not listed: it is asked for by its id until it is paid
/// for.
async fn list_jobs(State(service): State<Arc<Service>>) -> Json<Vec<Job>> {
    let queue = service.queue.lock().await;
    Json(
        placed(&queue.jobs)
            .into_iter()
            .filter(|job| job.state != JobState::AwaitingPayment)
            .collect(),
    )
}

/// One job, asked for by its id. An upload not yet paid for says only that
/// it is waiting for its payment: its title, and the wallet it names (which
/// may be anyone's), are not shown until that wallet has paid.
async fn one_job(
    State(service): State<Arc<Service>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Job>, StatusCode> {
    let queue = service.queue.lock().await;
    placed(&queue.jobs)
        .into_iter()
        .find(|job| job.id == id)
        .map(shown)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

/// A job as it is shown on its own: in full once paid for, and before then
/// no more than its id, its chain and that it waits for its payment.
fn shown(job: Job) -> Job {
    if job.state != JobState::AwaitingPayment {
        return job;
    }
    Job {
        title: String::new(),
        author: String::new(),
        source_hash: String::new(),
        sealed: false,
        ..job
    }
}

/// The queue as it stands now. A waiting job's position is counted when it is
/// asked for, so a queue that drains is a queue that visibly drains.
fn placed(jobs: &[Entry]) -> Vec<Job> {
    let mut waiting = 0;
    jobs.iter()
        .map(|entry| {
            let mut job = entry.job.clone();
            if matches!(job.state, JobState::Queued { .. }) {
                waiting += 1;
                job.state = JobState::Queued { position: waiting };
            }
            job
        })
        .collect()
}

// The queue ------------------------------------------------------------------

/// Forget the uploads never paid for, then take the oldest paid job, have it
/// reviewed, and build it.
async fn take_one(service: &Arc<Service>) -> Result<()> {
    if service.exposed.is_some() || !service.keeps_state() {
        return Ok(());
    }
    // Nothing of the last job's is left running: a sweep that left some, or
    // could not run, is tried again, and no job is taken until one is clean.
    service.swept().await;
    if isolation::unswept().is_some() {
        isolation::sweep_apart().await;
        if isolation::unswept().is_some() {
            return Ok(());
        }
        tracing::info!("the build user's processes are all killed: jobs are taken again");
    }
    let found = held_by(&service.jobs);
    let held = found.is_some();
    hold(service, found);
    if held {
        return Ok(());
    }
    let next = {
        let mut queue = service.queue.lock().await;
        let read_to = service.payments_read_to.lock().await.clone();
        if forget_unpaid(&mut queue, &read_to, &service.served(), now()) {
            service.sweep(&queue.jobs);
            let _ = write_queue(service, &mut queue).await;
        }
        // First come, first served, and one job in flight per author: nobody
        // can fill the queue with their own work.
        let busy: Vec<Wallet> = queue
            .jobs
            .iter()
            .filter(|entry| matches!(entry.job.state, JobState::Reviewing | JobState::Building))
            // A job caught mid-way on a chain not connected is not being
            // worked: it is picked up again once its chain connects, and
            // holds none of its author's jobs elsewhere until then.
            .filter(|entry| service.connected_to(entry.job.chain))
            .map(|entry| entry.author)
            .collect();
        // A job on a chain not yet connected waits for it, and one put back
        // while the reviewer was away waits its turn to ask again.
        let at = now();
        let next = queue.jobs.iter_mut().find(|entry| {
            matches!(entry.job.state, JobState::Queued { .. })
                && !busy.contains(&entry.author)
                && service.connected_to(entry.job.chain)
                && entry.not_before.is_none_or(|not_before| not_before <= at)
        });
        // Counted before it starts, and written down with the state it is
        // given next, so a restart knows how often it was taken.
        next.map(|entry| {
            entry.attempts += 1;
            entry.clone()
        })
    };
    let Some(entry) = next else {
        return Ok(());
    };
    let started = std::time::Instant::now();
    match run(service, &entry).await {
        Ok((state, bundle_hash)) => {
            set_state(service, &entry.job.id, state, bundle_hash, started).await
        }
        // Neither is the job's doing, nor the author's: it goes back in the
        // queue, its payment kept, and this take is not counted against it.
        Err(error) if error.downcast_ref::<Requeue>().is_some() => {
            if let Some(Requeue::Held(found)) = error.downcast_ref::<Requeue>() {
                hold(service, Some(found.clone()));
            }
            let reviewer_away = matches!(
                error.downcast_ref(),
                Some(Requeue::ReviewerAway(_) | Requeue::ReviewerOff(_) | Requeue::StoreAway(_))
            );
            // While a reviewer that was off is being woken, the job asks
            // again within the minute.
            let woken = matches!(error.downcast_ref(), Some(Requeue::ReviewerOff(_)));
            let mut queue = service.queue.lock().await;
            if let Some(waiting) = queue
                .jobs
                .iter_mut()
                .find(|waiting| waiting.job.id == entry.job.id)
            {
                waiting.job.state = JobState::Queued { position: 0 };
                waiting.attempts = waiting.attempts.saturating_sub(1);
                if reviewer_away {
                    let mut wait = waiting_for_the_reviewer(waiting.reviewer_away);
                    if woken {
                        wait = wait.min(time::Duration::seconds(60));
                    }
                    waiting.reviewer_away += 1;
                    waiting.not_before = Some(now() + wait);
                    tracing::warn!(
                        id = entry.job.id,
                        error = format!("{error:#}"),
                        seconds = wait.whole_seconds(),
                        "the reviewer did not answer: the job is back in the queue, its payment \
                         kept, and asks again later"
                    );
                } else {
                    tracing::warn!(
                        id = entry.job.id,
                        "the job is back in the queue, its payment kept, while the queue is held"
                    );
                }
            }
            let _ = write_queue(service, &mut queue).await;
        }
        // A node that didn't answer, or limited the rate of requests, is
        // neither the job's doing nor the author's: the job goes back in the
        // queue, its payment kept, and asks again later, as when the reviewer
        // is away. A registration that landed meanwhile is found then.
        Err(error) if chain::node_trouble(&error) => {
            let mut queue = service.queue.lock().await;
            if let Some(waiting) = queue
                .jobs
                .iter_mut()
                .find(|waiting| waiting.job.id == entry.job.id)
            {
                waiting.job.state = JobState::Queued { position: 0 };
                waiting.attempts = waiting.attempts.saturating_sub(1);
                let wait = waiting_for_the_reviewer(waiting.reviewer_away);
                waiting.reviewer_away += 1;
                waiting.not_before = Some(now() + wait);
                tracing::warn!(
                    id = entry.job.id,
                    error = format!("{error:#}"),
                    seconds = wait.whole_seconds(),
                    "the chain's node did not answer: the job is back in the queue, its payment \
                     kept, and asks again later"
                );
            }
            let _ = write_queue(service, &mut queue).await;
        }
        Err(error) => {
            // The fee is spent all the same: a free retry is something anyone
            // could trigger.
            tracing::warn!(
                id = entry.job.id,
                error = format!("{error:#}"),
                "a job failed on our side"
            );
            set_state(
                service,
                &entry.job.id,
                JobState::Failed {
                    outcome: Failure::Service,
                },
                None,
                started,
            )
            .await;
        }
    }
    Ok(())
}

/// One job, from review to registration. Returns how it ended, and the
/// bundle it made if it made one.
async fn run(service: &Arc<Service>, entry: &Entry) -> Result<(JobState, Option<String>)> {
    let chain = entry.job.chain;
    let strategy_id = entry.job.strategy_id.context("a queued job has an id")?;
    let started = std::time::Instant::now();
    let file = archive_file(&service.archives, &entry.job.id);
    let archive = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;

    // The review comes first: nothing is compiled, so no build script or
    // procedural macro runs, before the reviewer has read the source.
    set_state(service, &entry.job.id, JobState::Reviewing, None, started).await;
    // Only a verdict ends a paid job. Any other answer from the reviewer (it
    // is restarting, being deployed, not yet connected to the job's chain,
    // its model is down, its node refused a send: any status but success, a
    // body cut short or not a verdict, no answer at all) is our side's
    // trouble, not the job's: the job goes back in the queue.
    let reply = ask_for_review(
        service,
        chain,
        &ReviewRequest {
            strategy_id,
            archive_base64: shared::base64::encode(&archive),
            sealed: entry.job.sealed,
            manifest_path: entry.manifest_path.clone(),
        },
    )
    .await?;
    let status = reply.status();
    let body = reply.bytes().await.map_err(|error| {
        Requeue::ReviewerAway(format!("the reviewer's answer was cut short: {error}"))
    })?;
    if !status.is_success() {
        return Err(Requeue::ReviewerAway(format!(
            "the reviewer answered {status}: {}",
            String::from_utf8_lossy(&body[..body.len().min(500)])
        ))
        .into());
    }
    let answer: ReviewAnswer = serde_json::from_slice(&body).map_err(|error| {
        Requeue::ReviewerAway(format!("the reviewer's answer was not a verdict: {error}"))
    })?;
    let Some(clearance) = answer.clearance.clone() else {
        tracing::info!(id = entry.job.id, "the reviewer refused it");
        return Ok((JobState::Refused, None));
    };
    anyhow::ensure!(
        answer.source_hash == entry.job.source_hash,
        "the reviewer read different source"
    );

    set_state(service, &entry.job.id, JobState::Building, None, started).await;
    let job = Fresh(
        build::fresh(&service.jobs, &entry.job.id).context("a job's directory")?,
        service.clone(),
    );
    let source = job.0.join("source");
    shared::archive::unpack(&archive, &source)?;
    // What was checked before payment is checked again before anything is
    // compiled, as cheap as it is: the project's dependencies against the
    // allowlist as it stands now, and that it carries no Cargo config. A
    // project refused only for how it names the SDK passed when it was paid
    // for, so the SDK this build service carries has changed since: the
    // failure is ours, not the author's.
    let refused = build::overrides(&source)
        .map(|refused| (refused, Failure::Build))
        .or_else(|| {
            let refused =
                shared::dependencies::checked(&archive, &entry.manifest_path, &service.allowed)
                    .err()?;
            let ours = shared::dependencies::sdk_refused(&archive, &service.allowed).is_some();
            Some((
                refused,
                if ours {
                    Failure::Service
                } else {
                    Failure::Build
                },
            ))
        });
    if let Some((refused, outcome)) = refused {
        tracing::warn!(
            id = entry.job.id,
            refused,
            outcome = outcome.label(),
            "a paid project no longer passes the checks made before it was paid for"
        );
        return Ok((JobState::Failed { outcome }, None));
    }
    // And nothing lies above it that cargo would take for its own: should
    // something, the job goes back in the queue and the queue is held.
    if let Some(found) = build::above(&source) {
        return Err(Requeue::Held(found).into());
    }
    // What is registered is the manifest the reviewer read, read from the
    // archive itself and checked again, never the copy in the build
    // directory, which the project's own build code (a build script, a
    // procedural macro) can rewrite once the build runs.
    let manifest =
        build::manifest_in(&archive, &entry.manifest_path).map_err(|error| anyhow!(error))?;
    manifest
        .check(&service.catalogue, &service.network)
        .map_err(|error| anyhow!(error))?;
    anyhow::ensure!(
        manifest.vault_chain().map_err(|error| anyhow!(error))? == chain,
        "the manifest puts the vault on {}, and the job is for {chain}",
        manifest.vault.chain
    );

    let sdk = service.sdk.clone();
    let vendor = service.vendor.clone();
    let package = entry.package.clone();
    let at = source.clone();
    let built =
        tokio::task::spawn_blocking(move || build::build(&source, &sdk, &vendor, &package)).await?;
    // Found above the build only once it was about to compile: not the job's
    // doing either.
    if let (Err((Failure::Service, _)), Some(found)) = (&built, build::above(&at)) {
        return Err(Requeue::Held(found).into());
    }
    drop(job);
    let built = match built {
        Ok(built) => built,
        Err((outcome, _)) => {
            tracing::info!(
                id = entry.job.id,
                outcome = outcome.label(),
                "a build failed"
            );
            return Ok((JobState::Failed { outcome }, None));
        }
    };

    let bundle_hash = if chain == Chain::Solana {
        solana::register(
            service,
            entry,
            strategy_id,
            &archive,
            manifest,
            built,
            &answer,
        )
        .await?
    } else {
        register(
            service,
            entry,
            strategy_id,
            &archive,
            manifest,
            built,
            &clearance,
        )
        .await?
    };
    Ok((JobState::Ready, Some(bundle_hash)))
}

/// Ask the reviewer for a review, signed as the build service when it has a
/// key to sign with. A reviewer that can't be reached at all is asked to be
/// woken, and the job waits for it.
async fn ask_for_review(
    service: &Service,
    chain: Chain,
    request: &ReviewRequest,
) -> Result<reqwest::Response> {
    let url = reqwest::Url::parse(&format!("{}/{chain}/reviews", service.reviewer))
        .context("the reviewer's URL")?;
    let body = serde_json::to_vec(request)?;
    let signer = match (&service.key, &service.internal) {
        (Some(key), _) => Some(cloud::Signer::Key(key.clone())),
        (None, Some(internal)) => Some(internal.signer().clone()),
        (None, None) => None,
    };
    let mut asking = service
        .http
        .post(url.clone())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .timeout(Duration::from_secs(30 * 60));
    if let Some(signer) = signer {
        let at = cloud::now();
        let signature = signer
            .sign(cloud::request_digest("POST", url.path(), &body, at))
            .await?;
        asking = asking.header(cloud::AT_HEADER, at.to_string()).header(
            cloud::SIGNATURE_HEADER,
            alloy::hex::encode_prefixed(&signature),
        );
    }
    match asking.body(body).send().await {
        Ok(reply) => Ok(reply),
        Err(error) if error.is_connect() => {
            if let Some(internal) = &service.internal {
                match internal.wake("reviewer").await {
                    Ok(()) => tracing::info!("the reviewer is off: asked the API to wake it"),
                    Err(error) => tracing::warn!(
                        error = format!("{error:#}"),
                        "the reviewer is off, and could not be asked to wake"
                    ),
                }
            }
            Err(Requeue::ReviewerOff(format!("the reviewer could not be reached: {error}")).into())
        }
        Err(error) => {
            Err(Requeue::ReviewerAway(format!("the reviewer did not answer: {error}")).into())
        }
    }
}

/// Why a job taken from the queue goes back in it, its payment kept and the
/// take not counted against it: nothing the job or its author did.
#[derive(Debug)]
enum Requeue {
    /// The reviewer gave no verdict: it did not answer, or answered anything
    /// else.
    ReviewerAway(String),
    /// The reviewer could not be reached at all: it is off, and has been
    /// asked to wake.
    ReviewerOff(String),
    /// The API's store did not take the built files.
    StoreAway(String),
    /// Something lies above the job's build that cargo would take for its
    /// own (see [`held_by`]).
    Held(PathBuf),
}

impl std::fmt::Display for Requeue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReviewerAway(why) | Self::ReviewerOff(why) | Self::StoreAway(why) => {
                f.write_str(why)
            }
            Self::Held(found) => write!(f, "{} lies above the build", found.display()),
        }
    }
}

impl std::error::Error for Requeue {}

/// How long a job put back because the reviewer did not answer waits before
/// it asks again, `times` puts back before: ten seconds, doubling, never
/// more than ten minutes. Meanwhile the queue moves on to other jobs.
fn waiting_for_the_reviewer(times: u32) -> time::Duration {
    let seconds = 10u64.saturating_mul(1 << times.min(16));
    time::Duration::seconds(seconds.min(600) as i64)
}

/// A job's own directory, removed whenever the job is done with it, however
/// it ends: nothing a job unpacked or built is needed once it has ended.
/// Every process of the build user is killed first, so nothing of the job
/// runs on into the next (see [`isolation::sweep`]). Both block, so on the
/// async runtime they are done on a thread kept for blocking work, and the
/// next job waits for them (see [`Service::swept`]).
struct Fresh(PathBuf, Arc<Service>);

impl Drop for Fresh {
    fn drop(&mut self) {
        let directory = std::mem::take(&mut self.0);
        let done = move || {
            isolation::sweep();
            let _ = std::fs::remove_dir_all(&directory);
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                let sweeping = runtime.spawn_blocking(done);
                *self.1.sweeping.lock().unwrap_or_else(|e| e.into_inner()) = Some(sweeping);
            }
            Err(_) => done(),
        }
    }
}

/// Write what was built into the store, then put it on the registry, with
/// `manifest` (the one the reviewer read), the reviewer's go-ahead and the
/// builder's own signature. Returns the bundle the registry holds for the
/// strategy, which the store holds too: an error if it can't be both.
async fn register(
    service: &Arc<Service>,
    entry: &Entry,
    strategy_id: u64,
    archive: &[u8],
    manifest: Manifest,
    built: build::Built,
    clearance: &str,
) -> Result<String> {
    let chain = entry.job.chain;
    let on = service.on(chain)?;
    let author = entry.author.evm()?;
    let (canonical, manifest_hash) = canonical_of(service, entry, strategy_id, &manifest, &built);

    let chains: Vec<ChainNeed> = manifest
        .chains(&service.catalogue)
        .map_err(|error| anyhow!(error))?
        .into_iter()
        .map(|need| ChainNeed {
            chain: name(need.chain.name()),
            drivers: need.drivers.iter().map(|driver| name(driver)).collect(),
        })
        .collect();
    let bridges: Vec<B256> = manifest
        .bridge_names()
        .iter()
        .map(|bridge| name(bridge))
        .collect();
    let restricted: Vec<B256> = manifest
        .restricted(&service.catalogue)
        .iter()
        .map(|code| name(code))
        .collect();

    let bytes =
        |hash: &str| -> Result<B256> { Ok(B256::from(hash_to_bytes(hash).context("a hash")?)) };
    let (source_hash, bundle_hash, manifest_bytes) = (
        bytes(&entry.job.source_hash)?,
        bytes(&built.bundle_hash)?,
        bytes(&manifest_hash)?,
    );
    // The digest signed is worked out here, from what is registered, the
    // chain the client was configured for (checked against the node when it
    // connected) and the registry's address: a key signs whatever digest it
    // is handed, so one read from a node would be the node's to choose. The
    // registry's own is read too, and a difference is refused: the
    // registration would revert, and a node answering it so is not to be
    // trusted with the send either.
    let strategy = alloy::primitives::U256::from(strategy_id);
    let digest = chain::digest::registration(
        on.client.chain_id,
        on.deployments.strategies,
        &chain::digest::Registration {
            strategy_id: strategy,
            author,
            source_hash,
            bundle_hash,
            manifest_hash: manifest_bytes,
            chains: &chains,
            bridges: &bridges,
            restricted: &restricted,
        },
    );
    let theirs = on
        .client
        .read(
            on.deployments.strategies,
            IStrategyRegistry::registrationDigestCall {
                strategyId: strategy,
                author,
                sourceHash: source_hash,
                bundleHash: bundle_hash,
                manifestHash: manifest_bytes,
                chains: chains.clone(),
                bridges: bridges.clone(),
                restricted: restricted.clone(),
            },
        )
        .await?;
    if theirs != digest {
        tracing::error!(
            id = entry.job.id,
            %chain,
            ours = %digest,
            %theirs,
            "the registry's registration digest is not the one worked out here: not signed"
        );
        bail!("the registry's registration digest is not the one worked out here");
    }
    let signature = match &service.key {
        Some(key) => on.client.with_key(key.clone())?.sign(digest)?,
        None => on.client.sign(on.deployments.roles.builder, digest).await?,
    };

    keep_built(service, entry, archive, &built, &canonical, &manifest_hash).await?;

    let clearance: Bytes = clearance.parse().context("the reviewer's go-ahead")?;
    let register = IStrategyRegistry::registerCall {
        strategyId: alloy::primitives::U256::from(strategy_id),
        author,
        sourceHash: source_hash,
        bundleHash: bundle_hash,
        manifestHash: manifest_bytes,
        chains,
        bridges,
        restricted,
        builderSignature: signature,
        reviewerClearance: clearance,
    };
    let sent = match &service.key {
        Some(key) => {
            on.client
                .with_key(key.clone())?
                .send(on.deployments.strategies, register)
                .await
        }
        None => {
            on.client
                .send(
                    on.deployments.roles.builder,
                    on.deployments.strategies,
                    register,
                )
                .await
        }
    };
    // A registration whose receipt was not seen in time may land all the
    // same, and one that landed is refused if sent again: either way, the
    // registry says whether it holds it, and which bundle it holds: one
    // registered before a restart may be another build of the same source.
    let bundle_hash = match sent {
        Ok(_) => built.bundle_hash,
        Err(error) => match registered_bundle(&on, strategy_id).await {
            Ok(Some(held)) => {
                tracing::info!(
                    id = entry.job.id,
                    %error,
                    "the registration was not seen to go through, and the registry holds it"
                );
                held
            }
            _ => return Err(error).context("the registry would not take the registration"),
        },
    };
    // A job reads ready only with the bundle the registry holds, and only if
    // the store holds that bundle's files for anyone to read.
    anyhow::ensure!(
        service.holds(&bundle_hash),
        "the registry holds bundle {bundle_hash} for strategy {strategy_id}, and the store \
         does not hold its files, so the job has failed rather than read ready with it"
    );

    tracing::info!(
        id = entry.job.id,
        strategy_id,
        "built, signed and registered"
    );
    Ok(bundle_hash)
}

/// What a build is registered as: the canonical manifest, naming the
/// builder's image (in the attested image, the one the launcher attests it
/// runs; on a development chain, the name standing for it), and its hash.
fn canonical_of(
    service: &Service,
    entry: &Entry,
    strategy_id: u64,
    manifest: &Manifest,
    built: &build::Built,
) -> (Canonical, String) {
    let builder_image = service
        .image
        .unwrap_or_else(|| chain::attest::image_digest("builder"));
    let canonical = Canonical {
        strategy_id,
        vault_chain: entry.job.chain,
        author: entry.author.to_string(),
        bundle_hash: built.bundle_hash.clone(),
        component_hash: built.component_hash.clone(),
        source_hash: entry.job.source_hash.clone(),
        sealed: entry.job.sealed,
        world: build::WORLD.into(),
        toolchain: build::TOOLCHAIN.into(),
        builder_image: format!("{builder_image:#x}"),
        manifest: manifest.clone(),
    };
    let hash = canonical.hash();
    (canonical, hash)
}

/// Put what was built into the store, and off the machine into the API's,
/// before anything is registered.
async fn keep_built(
    service: &Service,
    entry: &Entry,
    archive: &[u8],
    built: &build::Built,
    canonical: &Canonical,
    manifest_hash: &str,
) -> Result<()> {
    // Everything a page shows about a build lives in the store, addressed by
    // the hashes the chain holds, written before the registration so the
    // registry never points at bytes nobody has.
    store::put(
        &service.store,
        &built.bundle_hash,
        "component.wasm",
        &built.component,
    )?;
    store::put(
        &service.store,
        &built.bundle_hash,
        "bundle.tar",
        &built.bundle,
    )?;
    store::put_json(&service.store, manifest_hash, "manifest.json", canonical)?;
    store::put_json(
        &service.store,
        &built.bundle_hash,
        "hashes.json",
        &serde_json::json!({
            "component": built.component_hash,
            "bundle": built.bundle_hash,
            "manifest": manifest_hash,
            "source": entry.job.source_hash,
            "toolchain": build::TOOLCHAIN,
            "seconds": built.seconds,
        }),
    )?;
    // The source is kept either way, because a rebuild has to be possible
    // for as long as the vault lives. A sealed one is kept under a name the
    // public store refuses to serve.
    store::put(
        &service.store,
        &entry.job.source_hash,
        if entry.job.sealed {
            "source.sealed.tar"
        } else {
            "source.tar"
        },
        archive,
    )?;
    // Off the machine, the API's store takes them, each checked against its
    // hash there, before anything is registered: the registry never names
    // bytes the API lacks. Sealed source never leaves.
    if let Some(internal) = &service.internal {
        let bundle_at = store::directory(Path::new(""), &built.bundle_hash);
        let manifest_at = store::directory(Path::new(""), manifest_hash);
        let source_at = store::directory(Path::new(""), &entry.job.source_hash);
        let mut pushes = vec![
            (bundle_at.clone(), "hashes.json"),
            (manifest_at, "manifest.json"),
            (bundle_at.clone(), "bundle.tar"),
            (bundle_at, "component.wasm"),
        ];
        if !entry.job.sealed {
            pushes.push((source_at, "source.tar"));
        }
        for (at, name) in pushes {
            let hash = at.to_string_lossy().to_string();
            let bytes = std::fs::read(service.store.join(&at).join(name))
                .with_context(|| format!("reading {hash}/{name} back"))?;
            internal
                .push_store(&hash, name, bytes)
                .await
                .map_err(|error| Requeue::StoreAway(format!("{error:#}")))?;
        }
    }

    Ok(())
}

/// A short name as the chain stores it: the text, then zeros.
fn name(text: &str) -> B256 {
    let mut out = [0u8; 32];
    let bytes = text.as_bytes();
    let take = bytes.len().min(32);
    out[..take].copy_from_slice(&bytes[..take]);
    B256::from(out)
}

async fn set_state(
    service: &Arc<Service>,
    id: &str,
    state: JobState,
    bundle_hash: Option<String>,
    started: std::time::Instant,
) {
    let mut queue = service.queue.lock().await;
    if let Some(entry) = queue.jobs.iter_mut().find(|entry| entry.job.id == id) {
        if state.is_finished() {
            entry.job.duration_seconds = started.elapsed().as_secs() as u32;
            // An ended job is never built again, and a built one's source is
            // in the store, so its archive is not kept.
            let _ = std::fs::remove_file(archive_file(&service.archives, id));
            service.let_go(vec![id.to_string()]);
        }
        entry.job.state = state;
        if let Some(hash) = bundle_hash {
            entry.job.bundle_hash = Some(hash);
        }
    }
    let _ = write_queue(service, &mut queue).await;
}

/// Keep the queue on disk, and off the machine when the state is kept there,
/// and note whether it is. A job that cannot be written down is still built;
/// the note about it is what would be lost. A payment's admission is the
/// exception: payments are not read past it until it is written down. So is
/// an upload: it is not answered until it is.
async fn write_queue(service: &Service, queue: &mut Queue) -> Result<()> {
    let written = match serde_json::to_vec(&*queue) {
        Ok(bytes) => service.keep(&service.queue_file, QUEUE, &bytes).await,
        Err(error) => Err(error.into()),
    };
    queue.unwritten = written.is_err();
    if let Err(error) = &written {
        tracing::warn!(
            error = format!("{error:#}"),
            "the queue could not be written down"
        );
    }
    written
}

/// Replace a file whole or not at all: written beside it, flushed to disk,
/// then renamed over it, so a machine that stops part way leaves the old
/// file or the new one, never the start of one.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let directory = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(directory)?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".new");
    let fresh = directory.join(name);
    let mut file = std::fs::File::create(&fresh)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&fresh, path)?;
    // The rename is kept once the directory is.
    std::fs::File::open(directory)?.sync_all()
}

/// The wall clock, in unix seconds.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn now() -> time::PrimitiveDateTime {
    let now = time::OffsetDateTime::now_utc();
    time::PrimitiveDateTime::new(now.date(), now.time())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build service with nothing connected, its files under `dir`, its
    /// state kept off the machine through `internal`.
    fn a_service(dir: &Path, internal: Option<cloud::Internal>) -> Service {
        Service {
            configured: vec![Chain::Base],
            connected: Default::default(),
            solana: Default::default(),
            solana_key: None,
            confirmations: BTreeMap::new(),
            network: "local".into(),
            catalogue: Catalogue::published(),
            store: dir.join("store"),
            builds: dir.join("builds"),
            jobs: dir.join("jobs"),
            sdk: dir.join("sdk"),
            vendor: dir.join("vendor"),
            allowed: shared::dependencies::Allowlist::platform()
                .carrying(
                    strategy_sdk::VERSION,
                    build::carries(
                        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../strategy-sdk"),
                    )
                    .unwrap(),
                )
                .unwrap(),
            reviewer: String::new(),
            http: reqwest::Client::new(),
            queue: Mutex::new(Queue::default()),
            queue_file: dir.join("store/queue.json"),
            archives: dir.join("store/archives"),
            payments_read: Mutex::new(BTreeMap::new()),
            payments_file: dir.join("store/payments.json"),
            payments_read_to: Mutex::new(BTreeMap::new()),
            held: std::sync::Mutex::new(None),
            exposed: None,
            canonical: None,
            image: None,
            key: None,
            attestation: None,
            internal,
            payments_seen: Mutex::new(BTreeMap::new()),
            payments_stalled: Default::default(),
            idle: cloud::Idle::new(None),
            turns: cloud::Turns::default(),
            sweeping: std::sync::Mutex::new(None),
        }
    }

    fn a_scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shonin-builder-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn internal_at(url: &str, key: [u8; 32]) -> cloud::Internal {
        cloud::Internal::new(
            url,
            "builder",
            cloud::Signer::Key(PrivateKeySigner::from_bytes(&B256::repeat_byte(9)).unwrap()),
            Arc::new(cloud::stand_in::Sealed(key)),
        )
        .unwrap()
    }

    /// An attestation whose blessing on every chain is as the test sets it.
    struct Blessing(std::sync::atomic::AtomicBool);

    impl cloud::Attesting for Blessing {
        fn serve_chain(&self, _: chain::Client, _: chain::Deployments) {}
        fn serve_cluster(&self, _: String, _: shonin_rootkey::service::ClusterServed) {}
        fn ready_cluster(&self, _: &str) -> bool {
            false
        }
        fn ready(&self, _: u64) -> bool {
            self.0.load(Ordering::SeqCst)
        }
        fn renew_at(&self) -> Option<u64> {
            None
        }
        fn busy(&self) -> bool {
            false
        }
        fn routes(&self) -> axum::Router {
            axum::Router::new()
        }
    }

    /// In the attested image no wake plan goes to the API while the key is
    /// not blessed on the canonical chain; once it is, the plan goes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_wake_plan_is_sent_until_the_key_is_blessed_on_the_canonical_chain() {
        let (url, api) = cloud::stand_in::serve().await;
        let dir = a_scratch("plan-blessed");
        let blessing = Arc::new(Blessing(false.into()));
        let mut service = a_service(&dir, Some(internal_at(&url, [1; 32])));
        service.attestation = Some(blessing.clone());
        service.canonical = Some(8453);
        // Idle at once: each watch looks once and returns.
        service.idle = cloud::Idle::new(Some(Duration::ZERO));

        watch_idle(&service).await;
        assert!(api.plans.lock().unwrap().is_empty());

        // Refused by the API all the same: said once, and not fatal.
        blessing.0.store(true, Ordering::SeqCst);
        *api.refuse.lock().unwrap() = Some(403);
        watch_idle(&service).await;
        assert!(api.plans.lock().unwrap().is_empty());

        *api.refuse.lock().unwrap() = None;
        watch_idle(&service).await;
        assert_eq!(api.plans.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// On a new image, with the queue kept under the old image's key: until
    /// the key is blessed the build service reads its state and writes none,
    /// a failed write does not stop it, and it takes no upload; once
    /// blessed, the old queue is set aside, the queue written, and uploads
    /// are taken.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_state_is_written_until_the_key_is_blessed_and_the_old_image_s_state_is_set_aside() {
        let (url, api) = cloud::stand_in::serve().await;
        let old = a_scratch("old-image");
        let service = a_service(&old, Some(internal_at(&url, [1; 32])));
        write_queue(&service, &mut *service.queue.lock().await)
            .await
            .unwrap();
        let sealed_before = api.blobs.lock().unwrap()["builder/queue"].clone();
        let writes_before = api.signers.lock().unwrap().len();

        let dir = a_scratch("new-image");
        let blessing = Arc::new(Blessing(false.into()));
        let internal = internal_at(&url, [2; 32]).writes_once({
            let blessing = blessing.clone();
            move || cloud::Attesting::ready(&*blessing, 8453)
        });
        let mut service = a_service(&dir, Some(internal));
        service.attestation = Some(blessing.clone());
        service.canonical = Some(8453);
        service.idle = cloud::Idle::new(Some(Duration::ZERO));
        restore(
            service.internal.as_ref().unwrap(),
            &service.queue_file,
            &service.payments_file,
            &service.archives,
        )
        .await
        .expect("a queue under another key does not stop the start");
        assert!(
            !service.keeps_state(),
            "uploads are taken before the blessing"
        );
        let failed = write_queue(&service, &mut *service.queue.lock().await)
            .await
            .unwrap_err();
        assert!(failed.is::<cloud::NotBlessed>(), "{failed:#}");
        assert!(service.queue.lock().await.unwritten);
        watch_idle(&service).await;
        assert_eq!(
            api.signers.lock().unwrap().len(),
            writes_before,
            "a write was sent before the key was blessed"
        );
        assert_eq!(api.blobs.lock().unwrap()["builder/queue"], sealed_before);

        blessing.0.store(true, Ordering::SeqCst);
        watch_idle(&service).await;
        assert!(service.keeps_state());
        assert!(!service.queue.lock().await.unwritten);
        let blobs = api.blobs.lock().unwrap();
        let aside: Vec<_> = blobs
            .iter()
            .filter(|(name, _)| name.starts_with("builder/queue-orphan-"))
            .collect();
        assert_eq!(aside.len(), 1, "the old queue was not set aside");
        assert_eq!(aside[0].1, &sealed_before);
        assert_ne!(
            blobs["builder/queue"], sealed_before,
            "the queue was not written"
        );
        drop(blobs);
        let _ = std::fs::remove_dir_all(&old);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// While no upload nor job is taken for want of the blessing, or of the
    /// old image's state set aside, health answers 503 saying which; once
    /// both are done, 200.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn health_is_unavailable_until_the_key_is_blessed_and_the_old_image_s_state_is_set_aside()
    {
        let (url, _api) = cloud::stand_in::serve().await;
        let old = a_scratch("health-old-image");
        let service = a_service(&old, Some(internal_at(&url, [1; 32])));
        write_queue(&service, &mut *service.queue.lock().await)
            .await
            .unwrap();

        let dir = a_scratch("health-new-image");
        let blessing = Arc::new(Blessing(false.into()));
        let internal = internal_at(&url, [2; 32]).writes_once({
            let blessing = blessing.clone();
            move || cloud::Attesting::ready(&*blessing, 8453)
        });
        let mut service = a_service(&dir, Some(internal));
        service.attestation = Some(blessing.clone());
        service.canonical = Some(8453);
        service.idle = cloud::Idle::new(Some(Duration::ZERO));
        restore(
            service.internal.as_ref().unwrap(),
            &service.queue_file,
            &service.payments_file,
            &service.archives,
        )
        .await
        .unwrap();
        let service = Arc::new(service);

        let (status, said) = health(State(service.clone())).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{said}");
        assert!(said.contains("not blessed"), "{said}");

        // Blessed, with the old queue not yet set aside.
        blessing.0.store(true, Ordering::SeqCst);
        let (status, said) = health(State(service.clone())).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{said}");
        assert!(said.contains("another key"), "{said}");

        watch_idle(&service).await;
        assert!(service.keeps_state());
        let (status, said) = health(State(service.clone())).await;
        assert_eq!(status, StatusCode::OK, "{said}");
        let _ = std::fs::remove_dir_all(&old);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What an upload is answered for (its archive and the queue naming it)
    /// is kept off the machine before the answer, and a build service started
    /// afresh on an empty disk brings it back: the queue, where payments were
    /// read to, and the archive.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn state_written_before_an_upload_is_answered_survives_a_restart() {
        let (url, api) = cloud::stand_in::serve().await;
        let before = a_scratch("before");
        let service = a_service(&before, Some(internal_at(&url, [1; 32])));
        let author = Address::repeat_byte(4);
        service
            .keep(
                &archive_file(&service.archives, "8800"),
                &archive_name("8800"),
                b"the archive",
            )
            .await
            .unwrap();
        service
            .keep(&service.payments_file, PAYMENTS, br#"{"base":42}"#)
            .await
            .unwrap();
        {
            let mut queue = service.queue.lock().await;
            queue.next_id = 8801;
            queue.jobs.push(an_entry(
                author,
                "sha256:aa",
                JobState::Queued { position: 0 },
            ));
            write_queue(&service, &mut queue).await.unwrap();
        }
        assert!(api.blobs.lock().unwrap().contains_key("builder/queue"));

        // A fresh disk.
        let after = a_scratch("after");
        let restarted = a_service(&after, Some(internal_at(&url, [1; 32])));
        restore(
            restarted.internal.as_ref().unwrap(),
            &restarted.queue_file,
            &restarted.payments_file,
            &restarted.archives,
        )
        .await
        .unwrap();
        let queue: Queue = read_kept::<Written>(&restarted.queue_file)
            .unwrap()
            .map(Queue::from)
            .unwrap();
        assert_eq!(queue.next_id, 8801);
        assert_eq!(queue.jobs.len(), 1);
        assert_eq!(
            std::fs::read(archive_file(&restarted.archives, "8800")).unwrap(),
            b"the archive"
        );
        assert_eq!(
            std::fs::read(&restarted.payments_file).unwrap(),
            br#"{"base":42}"#
        );
        let _ = std::fs::remove_dir_all(&before);
        let _ = std::fs::remove_dir_all(&after);
    }

    /// An upload whose queue can't be kept off the machine is turned away,
    /// and the queue in memory is as it was before it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_upload_the_api_would_not_keep_is_turned_away() {
        let (url, api) = cloud::stand_in::serve().await;
        let dir = a_scratch("refused");
        let service = a_service(&dir, Some(internal_at(&url, [1; 32])));
        *api.refuse.lock().unwrap() = Some(503);
        let mut queue = service.queue.lock().await;
        let before = (
            queue.jobs.clone(),
            queue.forgotten.clone(),
            queue.credits.clone(),
        );
        queue.jobs.push(an_entry(
            Address::repeat_byte(4),
            "sha256:aa",
            JobState::AwaitingPayment,
        ));
        let refused = written_down(&service, &mut queue, "8800", before).await;
        assert_eq!(refused.unwrap_err().0, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            queue.jobs.is_empty(),
            "the turned-away upload is still queued"
        );
        drop(queue);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A chain whose reads are failing with a payment that may wait there is
    /// left out of the wake plan's counts, and gives the plan the time it is
    /// read again, so the waker starts the build service by then; a chain
    /// read as usual stays in the plan with no retry time.
    #[tokio::test]
    async fn a_chain_whose_payments_cannot_be_read_gives_the_wake_plan_a_time_to_try_again() {
        let dir = a_scratch("plan-stalled");
        let service = a_service(&dir, None);
        service
            .payments_seen
            .lock()
            .await
            .extend([("998".to_string(), 3), ("84532".to_string(), 1)]);
        let now = 1_000_000;
        let mut unread = Unread::default();
        unread.waiting(
            "998".to_string(),
            Some(Stalled {
                retry_at: now + 900,
                paused: false,
            }),
            now,
        );
        let left = plan(&service, false, &unread).await;
        assert_eq!(
            left.payments_seen,
            BTreeMap::from([("84532".to_string(), 1)])
        );
        assert_eq!(left.retry_at, Some(now + 900));
        let read = plan(&service, false, &Unread::default()).await;
        assert_eq!(read.payments_seen.len(), 2);
        assert_eq!(read.retry_at, None);
    }

    /// A payment that may wait behind a failed read keeps the build service
    /// up only while the read is tried again soon, and only when the read
    /// failed rather than the registry's record proving nothing; otherwise
    /// it is left to the plan's retry time, the earliest of all the chains'.
    #[test]
    fn a_payment_behind_a_failed_read_is_work_in_hand_only_while_it_is_read_again_soon() {
        let now = 1_000_000;
        let stalled = |retry_at: u64, paused: bool| Some(Stalled { retry_at, paused });
        let weighed = |chains: &[Option<Stalled>]| {
            let mut unread = Unread::default();
            for (at, stalled) in chains.iter().enumerate() {
                unread.waiting(at.to_string(), *stalled, now);
            }
            unread
        };
        // Read as usual: work in hand, no retry time.
        let unread = weighed(&[None]);
        assert!(unread.now && unread.stalled.is_empty() && unread.retry_at.is_none());
        // A brief failure: tried again soon, in this run.
        let unread = weighed(&[stalled(now + 5, false)]);
        assert!(unread.now, "{unread:?}");
        assert_eq!(unread.retry_at, Some(now + 5));
        // Failing a while: not kept up for, but woken for.
        let unread = weighed(&[stalled(now + 600, false), stalled(now + 300, false)]);
        assert!(!unread.now, "{unread:?}");
        assert_eq!(unread.retry_at, Some(now + 300));
        assert_eq!(unread.stalled.len(), 2);
        // The record proves nothing: never kept up for, still woken for.
        let unread = weighed(&[stalled(now + 5, true)]);
        assert!(!unread.now, "{unread:?}");
        assert_eq!(unread.retry_at, Some(now + 5));
    }

    /// The build service is busy with a paid job waiting, being reviewed or
    /// built, or a payment unread; an upload waiting for its payment, or a
    /// job that has ended, keeps it from nothing.
    #[tokio::test]
    async fn the_build_service_is_idle_only_with_nothing_to_do() {
        let dir = a_scratch("idle");
        let service = a_service(&dir, None);
        let author = Address::repeat_byte(4);
        assert!(!service.busy(false).await);
        assert!(service.busy(true).await, "idle with a payment unread");
        for (state, busy) in [
            (JobState::AwaitingPayment, false),
            (JobState::Ready, false),
            (JobState::Refused, false),
            (JobState::Queued { position: 0 }, true),
            (JobState::Reviewing, true),
            (JobState::Building, true),
        ] {
            *service.queue.lock().await = Queue::default();
            service
                .queue
                .lock()
                .await
                .jobs
                .push(an_entry(author, "sha256:aa", state.clone()));
            assert_eq!(service.busy(false).await, busy, "{state:?}");
        }
    }

    #[test]
    fn a_name_becomes_the_bytes_the_chain_holds() {
        let bytes = name("local-bridge");
        assert_eq!(&bytes[..12], b"local-bridge");
        assert_eq!(bytes[12], 0);
    }

    #[test]
    fn a_wallet_reads_as_its_chain_writes_it_and_a_queue_written_before_reads_as_it_was() {
        let evm = Address::repeat_byte(0xa1);
        let key = chain::solana::Pubkey::new_from_array([0xb2; 32]);
        let on_evm = Wallet::on(Chain::Base, &format!("{evm:#x}")).unwrap();
        let on_solana = Wallet::on(Chain::Solana, &key.to_string()).unwrap();
        assert_eq!(on_evm, Wallet::Evm(evm));
        assert_eq!(on_solana, Wallet::Solana(key.to_bytes()));
        // An address names no wallet on the other chain.
        assert!(Wallet::on(Chain::Solana, &format!("{evm:#x}")).is_err());
        assert!(Wallet::on(Chain::Base, &key.to_string()).is_err());
        // Written as each chain writes it, and read back the same.
        for wallet in [on_evm, on_solana] {
            let text = serde_json::to_string(&wallet).unwrap();
            assert_eq!(serde_json::from_str::<Wallet>(&text).unwrap(), wallet);
        }
        assert_eq!(on_solana.to_string(), key.to_string());
        // A queue written before Solana was served, its authors checksummed.
        let written = serde_json::json!(format!("{evm}"));
        assert_eq!(serde_json::from_value::<Wallet>(written).unwrap(), on_evm);
        // Each pays only on its own chain.
        assert!(on_solana.evm().is_err() && on_evm.solana().is_err());
        assert_eq!(on_solana.solana().unwrap(), key);
    }

    #[tokio::test]
    async fn a_solana_payment_admits_only_the_upload_its_own_wallet_named() {
        let dir = a_scratch("solana-payment");
        let service = Arc::new(a_service(&dir, None));
        let key = chain::solana::Pubkey::new_from_array([0xb2; 32]);
        let mut entry = an_entry(Address::ZERO, "sha256:01", JobState::AwaitingPayment);
        entry.job.chain = Chain::Solana;
        entry.author = Wallet::Solana(key.to_bytes());
        *service.queue.lock().await = a_queue(vec![entry]);
        // The same upload paid from another wallet, or on another chain by
        // the same bytes, admits nothing.
        for (chain, author) in [
            (Chain::Solana, Wallet::Solana([0xc3; 32])),
            (Chain::Base, Wallet::Solana(key.to_bytes())),
        ] {
            admit_payments(
                &service,
                chain,
                vec![Payment {
                    strategy_id: 4,
                    upload: B256::repeat_byte(7),
                    author,
                }],
            )
            .await
            .unwrap();
            let queue = service.queue.lock().await;
            assert_eq!(queue.jobs[0].job.state, JobState::AwaitingPayment);
        }
        admit_payments(
            &service,
            Chain::Solana,
            vec![Payment {
                strategy_id: 5,
                upload: B256::repeat_byte(7),
                author: Wallet::Solana(key.to_bytes()),
            }],
        )
        .await
        .unwrap();
        let queue = service.queue.lock().await;
        assert_eq!(queue.jobs[0].job.strategy_id, Some(5));
        assert!(matches!(queue.jobs[0].job.state, JobState::Queued { .. }));
        drop(queue);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn an_entry(author: Address, source_hash: &str, state: JobState) -> Entry {
        Entry {
            job: Job {
                id: "8800".to_string(),
                chain: Chain::Base,
                strategy_id: None,
                title: "an agent".to_string(),
                author: format!("{author:#x}"),
                state,
                submitted: now(),
                duration_seconds: 0,
                source_hash: source_hash.to_string(),
                sealed: false,
                bundle_hash: None,
                review: None,
                strategy_slug: None,
            },
            upload: B256::repeat_byte(7),
            author: author.into(),
            archive: String::new(),
            manifest_path: "shonin.toml".to_string(),
            package: "agent".to_string(),
            attempts: 0,
            reviewer_away: 0,
            not_before: None,
        }
    }

    fn a_queue(jobs: Vec<Entry>) -> Queue {
        Queue {
            jobs,
            ..Queue::default()
        }
    }

    /// The chains served, in these tests.
    const BASE: &[Chain] = &[Chain::Base];

    /// Past either bound on uploads waiting for their payment, the oldest
    /// waiting under it is forgotten, leaving a note of itself, and the
    /// newest is kept: a wallet's own, or everyone's, never anyone else's
    /// and never one paid for.
    #[test]
    fn past_a_bound_the_oldest_upload_waiting_is_forgotten() {
        let named = Address::repeat_byte(0xa1);
        let other = Address::repeat_byte(0xb2);
        let mut queue = a_queue(vec![
            an_entry(other, "sha256:00", JobState::AwaitingPayment),
            an_entry(named, "sha256:01", JobState::AwaitingPayment),
            an_entry(named, "sha256:02", JobState::Queued { position: 0 }),
            an_entry(named, "sha256:03", JobState::AwaitingPayment),
            an_entry(named, "sha256:04", JobState::AwaitingPayment),
        ]);
        let at = now();
        assert!(!bound_unpaid(&mut queue, named.into(), at));
        queue
            .jobs
            .push(an_entry(named, "sha256:05", JobState::AwaitingPayment));
        assert!(bound_unpaid(&mut queue, named.into(), at));
        let kept: Vec<&str> = queue
            .jobs
            .iter()
            .map(|entry| &*entry.job.source_hash)
            .collect();
        assert_eq!(
            kept,
            [
                "sha256:00",
                "sha256:02",
                "sha256:03",
                "sha256:04",
                "sha256:05"
            ]
        );
        assert_eq!(queue.forgotten.len(), 1);
        assert_eq!(queue.forgotten[0].source_hash, "sha256:01");

        let mut queue = a_queue(
            (0..UNPAID_AT_ALL + 1)
                .map(|n| {
                    an_entry(
                        Address::with_last_byte(n as u8),
                        &format!("sha256:{n:02}"),
                        JobState::AwaitingPayment,
                    )
                })
                .collect(),
        );
        assert!(bound_unpaid(
            &mut queue,
            Address::with_last_byte(UNPAID_AT_ALL as u8).into(),
            at
        ));
        assert_eq!(queue.jobs.len(), UNPAID_AT_ALL);
        assert_eq!(queue.jobs[0].job.source_hash, "sha256:01");
        assert_eq!(queue.forgotten[0].source_hash, "sha256:00");
    }

    /// An upload on a chain no longer served, whose payments nobody reads,
    /// is forgotten a day after it arrived by the clock alone, leaving a
    /// note of itself in case the chain is served again.
    #[test]
    fn an_upload_on_a_chain_no_longer_served_is_forgotten_after_a_day() {
        let author = Address::repeat_byte(0xa1);
        let mut queue = a_queue(vec![
            an_entry(author, "sha256:01", JobState::AwaitingPayment),
            an_entry(author, "sha256:02", JobState::AwaitingPayment),
        ]);
        let at = now();
        queue.jobs[0].job.submitted = at - time::Duration::hours(25);
        queue.jobs[1].job.submitted = at - time::Duration::hours(23);
        assert!(forget_unpaid(
            &mut queue,
            &BTreeMap::new(),
            &[Chain::Robinhood],
            at
        ));
        assert_eq!(queue.jobs.len(), 1);
        assert_eq!(queue.jobs[0].job.source_hash, "sha256:02");
        assert_eq!(queue.forgotten[0].source_hash, "sha256:01");
    }

    /// A job a restart caught mid-way whose strategy the registry already
    /// holds is ready, with the bundle registered, if the store holds that
    /// bundle's files, and failed if it does not; any other is queued again.
    #[test]
    fn a_job_registered_before_a_restart_is_ready() {
        let author = Address::repeat_byte(0xa1);
        let mut jobs = vec![
            an_entry(author, "sha256:01", JobState::Building),
            an_entry(author, "sha256:02", JobState::Building),
            an_entry(author, "sha256:03", JobState::Reviewing),
        ];
        jobs[1].job.id = "8801".to_string();
        jobs[2].job.id = "8802".to_string();
        let bundle = format!("sha256:{}", "42".repeat(32));
        let unstored = format!("sha256:{}", "43".repeat(32));
        picked_up(
            &mut jobs,
            Chain::Base,
            &BTreeMap::from([
                ("8800".to_string(), bundle.clone()),
                ("8802".to_string(), unstored),
            ]),
            |hash| hash == bundle,
        );
        assert_eq!(jobs[0].job.state, JobState::Ready);
        assert_eq!(jobs[0].job.bundle_hash, Some(bundle));
        assert_eq!(jobs[1].job.state, JobState::Queued { position: 0 });
        assert_eq!(
            jobs[2].job.state,
            JobState::Failed {
                outcome: Failure::Service
            }
        );
        assert_eq!(jobs[2].job.bundle_hash, None);
    }

    /// An upload not yet paid for, asked for by its id, says only that it
    /// waits for its payment: not its title, nor the wallet it names.
    #[test]
    fn an_upload_not_yet_paid_for_shows_only_that_it_waits() {
        let author = Address::repeat_byte(0xa1);
        let waiting = shown(an_entry(author, "sha256:01", JobState::AwaitingPayment).job);
        assert_eq!(waiting.state, JobState::AwaitingPayment);
        assert_eq!(waiting.id, "8800");
        assert_eq!((waiting.title.as_str(), waiting.author.as_str()), ("", ""));
        let paid = shown(an_entry(author, "sha256:01", JobState::Queued { position: 1 }).job);
        assert_eq!(paid.title, "an agent");
        assert_eq!(paid.author, format!("{author:#x}"));
    }

    /// What is remembered of an upload forgotten on a chain no longer
    /// served, whose payments nobody reads, is let go a week after it was
    /// forgotten by the clock alone.
    #[test]
    fn a_forgotten_upload_on_a_chain_no_longer_served_is_let_go_after_a_week() {
        let author = Address::repeat_byte(0xa1);
        let mut queue = a_queue(vec![an_entry(
            author,
            "sha256:01",
            JobState::AwaitingPayment,
        )]);
        let at = now();
        queue.jobs[0].job.submitted = at - time::Duration::days(2);
        let unserved = [Chain::Robinhood];
        assert!(forget_unpaid(&mut queue, &BTreeMap::new(), &unserved, at));
        assert_eq!(queue.forgotten.len(), 1);
        let within = at + time::Duration::days(6);
        assert!(!forget_unpaid(
            &mut queue,
            &BTreeMap::new(),
            &unserved,
            within
        ));
        assert_eq!(queue.forgotten.len(), 1);
        let later = at + time::Duration::days(7);
        assert!(forget_unpaid(
            &mut queue,
            &BTreeMap::new(),
            &unserved,
            later
        ));
        assert!(queue.forgotten.is_empty());
    }

    /// An upload is forgotten once it has waited a day for its payment, by
    /// the clock and in the chain time payments have been read through,
    /// leaving a note of itself; a younger one, or one paid for however old,
    /// is kept.
    #[test]
    fn an_upload_waiting_a_day_for_its_payment_is_forgotten() {
        let author = Address::repeat_byte(0xa1);
        let mut queue = a_queue(vec![
            an_entry(author, "sha256:01", JobState::AwaitingPayment),
            an_entry(author, "sha256:02", JobState::AwaitingPayment),
            an_entry(author, "sha256:03", JobState::Queued { position: 0 }),
        ]);
        let at = now();
        queue.jobs[0].job.submitted = at - time::Duration::hours(25);
        queue.jobs[1].job.submitted = at - time::Duration::hours(23);
        queue.jobs[2].job.submitted = at - time::Duration::days(2);
        let read_to = BTreeMap::from([(Chain::Base, at)]);
        assert!(forget_unpaid(&mut queue, &read_to, BASE, at));
        let kept: Vec<&str> = queue
            .jobs
            .iter()
            .map(|entry| &*entry.job.source_hash)
            .collect();
        assert_eq!(kept, ["sha256:02", "sha256:03"]);
        let noted: Vec<&str> = queue
            .forgotten
            .iter()
            .map(|forgotten| &*forgotten.source_hash)
            .collect();
        assert_eq!(noted, ["sha256:01"]);
        assert_eq!(queue.forgotten[0].upload, B256::repeat_byte(7));
        assert!(!forget_unpaid(&mut queue, &read_to, BASE, at));
    }

    /// While the payments read on a chain are not yet a day past an upload,
    /// it is not forgotten however long the clock says it has waited: its
    /// payment may be in what is still to be read.
    #[test]
    fn an_upload_is_not_forgotten_while_its_payment_may_be_unread() {
        let author = Address::repeat_byte(0xa1);
        let mut queue = a_queue(vec![an_entry(
            author,
            "sha256:01",
            JobState::AwaitingPayment,
        )]);
        let at = now();
        queue.jobs[0].job.submitted = at - time::Duration::days(3);
        let behind = BTreeMap::from([(
            Chain::Base,
            at - time::Duration::days(2) - time::Duration::minutes(1),
        )]);
        assert!(!forget_unpaid(&mut queue, &BTreeMap::new(), BASE, at));
        assert!(!forget_unpaid(&mut queue, &behind, BASE, at));
        let elsewhere = BTreeMap::from([(Chain::Robinhood, at)]);
        assert!(!forget_unpaid(&mut queue, &elsewhere, BASE, at));
        assert_eq!(queue.jobs.len(), 1);
        assert!(queue.forgotten.is_empty());

        // Read far enough, but the clock not yet a day on: still kept.
        queue.jobs[0].job.submitted = at - time::Duration::hours(23);
        let ahead = BTreeMap::from([(Chain::Base, at + time::Duration::days(2))]);
        assert!(!forget_unpaid(&mut queue, &ahead, BASE, at));
        assert_eq!(queue.jobs.len(), 1);
    }

    /// A forgotten upload is remembered for a week, by the clock and in the
    /// chain time payments have been read through, then no longer.
    #[test]
    fn a_forgotten_upload_is_remembered_for_a_week() {
        let author = Address::repeat_byte(0xa1);
        let mut queue = a_queue(vec![an_entry(
            author,
            "sha256:01",
            JobState::AwaitingPayment,
        )]);
        let at = now();
        queue.jobs[0].job.submitted = at - time::Duration::days(2);
        let read_to = BTreeMap::from([(Chain::Base, at)]);
        assert!(forget_unpaid(&mut queue, &read_to, BASE, at));
        assert_eq!(queue.forgotten.len(), 1);

        let later = at + time::Duration::days(8);
        let behind = BTreeMap::from([(Chain::Base, at + time::Duration::days(6))]);
        assert!(!forget_unpaid(&mut queue, &behind, BASE, later));
        assert_eq!(queue.forgotten.len(), 1);
        let caught_up = BTreeMap::from([(Chain::Base, later)]);
        assert!(!forget_unpaid(
            &mut queue,
            &caught_up,
            BASE,
            at + time::Duration::days(6)
        ));
        assert_eq!(queue.forgotten.len(), 1);
        assert!(forget_unpaid(&mut queue, &caught_up, BASE, later));
        assert!(queue.forgotten.is_empty());
    }

    /// An upload sent again before it is paid for (a retry after an answer
    /// that never arrived) is the one already waiting, so it can't be paid
    /// for twice; anyone else's, other source, or one already paid for is a
    /// job of its own.
    #[test]
    fn an_upload_sent_again_before_it_is_paid_for_is_the_one_already_waiting() {
        let author = Address::repeat_byte(0xa1);
        let waiting = vec![an_entry(author, "sha256:01", JobState::AwaitingPayment)];
        let found = waiting_for_payment(
            &waiting,
            Chain::Base,
            author.into(),
            "sha256:01",
            false,
            "shonin.toml",
            "agent",
        );
        assert_eq!(found.map(|entry| entry.upload), Some(B256::repeat_byte(7)));

        let again = |jobs: &[Entry], chain, author: Address, hash: &str, sealed| {
            waiting_for_payment(
                jobs,
                chain,
                author.into(),
                hash,
                sealed,
                "shonin.toml",
                "agent",
            )
            .is_some()
        };
        assert!(!again(
            &waiting,
            Chain::Base,
            Address::repeat_byte(0xb2),
            "sha256:01",
            false
        ));
        assert!(!again(&waiting, Chain::Base, author, "sha256:02", false));
        assert!(!again(&waiting, Chain::Base, author, "sha256:01", true));
        assert!(!again(
            &waiting,
            Chain::Robinhood,
            author,
            "sha256:01",
            false
        ));
        let paid = vec![an_entry(author, "sha256:01", JobState::Reviewing)];
        assert!(!again(&paid, Chain::Base, author, "sha256:01", false));
    }
}
