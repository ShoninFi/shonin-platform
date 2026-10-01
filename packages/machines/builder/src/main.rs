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

mod build;
mod store;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, B256, Bytes, keccak256};
use alloy::rpc::types::Filter;
use alloy::sol_types::SolEvent;
use anyhow::{Context, Result, anyhow};
use axum::Json;
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
    /// Where submissions are unpacked and compiled.
    #[arg(long)]
    builds: PathBuf,
    /// The SDK this build service carries, which every submission is built
    /// against whatever it asked for.
    #[arg(long)]
    sdk: PathBuf,
    /// The reviewer, which reads every submission before it is built.
    #[arg(long, env = "SHONIN_REVIEWER_URL")]
    reviewer_url: String,
    #[arg(long, default_value_t = 19_084)]
    port: u16,
}

struct Service {
    /// Every vault chain it builds for: an agent is published on its vault
    /// chain.
    chains: Vec<Served>,
    /// `local`, `testnet` or `mainnet`: which drivers are on offer.
    network: String,
    catalogue: Catalogue,
    store: PathBuf,
    builds: PathBuf,
    sdk: PathBuf,
    reviewer: String,
    http: reqwest::Client,
    /// The queue, oldest first.
    jobs: Mutex<Vec<Entry>>,
    /// Where the queue is kept, so restarting the service does not lose a
    /// job someone paid for.
    queue_file: PathBuf,
}

impl Service {
    fn on(&self, chain: Chain) -> Result<&Served> {
        self.chains
            .iter()
            .find(|served| served.vault_chain == chain)
            .ok_or_else(|| anyhow!("this build service does not serve {chain}"))
    }
}

/// A job, and what it was asked to build.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Entry {
    job: Job,
    /// The reference the author pays against.
    upload: B256,
    /// The wallet the upload named: only its payment starts the job.
    author: Address,
    /// The project, base64, as it arrived, so a restart can still build it.
    archive: String,
    /// Where the agent's `shonin.toml` is, inside the project.
    manifest_path: String,
    /// The cargo package to build.
    package: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    let network = args.chain_args.network.to_string();
    let chains = args.chain_args.connect_all().await?;
    for served in &chains {
        bless(&served.client, &served.deployments).await?;
    }

    // A job someone paid for should survive this service restarting, so the
    // queue is on disk rather than only in memory.
    let queue_file = args.store.join("queue.json");
    let waiting: Vec<Entry> = std::fs::read(&queue_file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    if !waiting.is_empty() {
        tracing::info!(count = waiting.len(), "picked the queue back up");
    }

    let service = Arc::new(Service {
        chains,
        network,
        catalogue: Catalogue::published(),
        store: args.store,
        builds: args.builds,
        sdk: args.sdk,
        reviewer: args.reviewer_url.trim_end_matches('/').to_string(),
        http: reqwest::Client::new(),
        jobs: Mutex::new(waiting),
        queue_file,
    });

    // Payments, on every chain.
    for index in 0..service.chains.len() {
        let watcher = service.clone();
        tokio::spawn(async move {
            let mut from = watcher.chains[index].deployments.deployed_block;
            loop {
                match admit(&watcher, index, from).await {
                    Ok(next) => from = next,
                    Err(error) => tracing::warn!(%error, "could not read payments"),
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }

    let worker = service.clone();
    tokio::spawn(async move {
        loop {
            if let Err(error) = take_one(&worker).await {
                tracing::warn!(%error, "the queue stalled");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    let app = axum::Router::new()
        .route("/health", get(|| async { "builder" }))
        .route("/jobs", get(list_jobs))
        .route("/jobs/{id}", get(one_job))
        .route("/{chain}/uploads", post(upload))
        .with_state(service);

    let address = SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "the build service is open");
    axum::serve(listener, app).await?;
    Ok(())
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

/// Take a project, check what can be checked before anyone pays, and hand
/// back the reference to pay against. A manifest the platform won't accept,
/// or source over the limit, is refused here, before the $1 is spent.
async fn upload(
    State(service): State<Arc<Service>>,
    UrlPath(chain): UrlPath<Chain>,
    Json(upload): Json<Upload>,
) -> Result<Json<UploadAccepted>, Refusal> {
    service.on(chain).map_err(|error| bad(error.to_string()))?;
    let author: Address = upload
        .author
        .parse()
        .map_err(|_| bad("the author is not an address"))?;
    let archive = shared::base64::decode(&upload.archive_base64).map_err(bad)?;
    let source_hash = hash_bytes(&archive);

    // Read it the way the build will, in a directory of its own.
    let scratch = service
        .builds
        .join(format!("upload-{}", &source_hash[7..23]));
    let _ = std::fs::remove_dir_all(&scratch);
    shared::archive::unpack(&archive, &scratch)
        .map_err(|error| bad(format!("the archive: {error}")))?;
    let checked = check(&service, &scratch, upload.package.as_deref(), chain);
    let _ = std::fs::remove_dir_all(&scratch);
    let (manifest, manifest_path, package) = checked.map_err(bad)?;

    let mut jobs = service.jobs.lock().await;
    let id = format!("{}", 8_800 + jobs.len());
    // Unique to this author, this source and this job, so a second upload of
    // the same project is a second job with its own reference.
    let reference = keccak256(
        [
            author.as_slice(),
            &hash_to_bytes(&source_hash).unwrap_or_default(),
            id.as_bytes(),
        ]
        .concat(),
    );
    let job = Job {
        id: id.clone(),
        chain,
        strategy_id: None,
        title: manifest.description.title.clone(),
        author: format!("{author:#x}"),
        state: JobState::AwaitingPayment,
        submitted: now(),
        duration_seconds: 0,
        source_hash,
        sealed: upload.sealed,
        bundle_hash: None,
        review: None,
        strategy_slug: None,
    };
    jobs.push(Entry {
        job,
        upload: reference,
        author,
        archive: upload.archive_base64,
        manifest_path,
        package,
    });
    write_queue(&service, &jobs);
    tracing::info!(id, "an upload is waiting for its payment");
    Ok(Json(UploadAccepted {
        upload: format!("{reference:#x}"),
        job_id: id,
    }))
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
    let manifest = build::read_manifest(&path)?;
    manifest.check(&service.catalogue, &service.network)?;
    if manifest.vault_chain()? != chain {
        return Err(format!(
            "shonin.toml puts the vault on {}, and this upload is for {chain}",
            manifest.vault.chain
        ));
    }
    let package = build::package_of(&path).ok_or("no Cargo.toml beside shonin.toml")?;
    let relative = path
        .strip_prefix(source)
        .map_err(|_| "the manifest is outside the project")?
        .to_string_lossy()
        .to_string();
    Ok((manifest, relative, package))
}

/// Read the payments on one chain since `from`, and admit the jobs they pay
/// for. A payment starts a job only if it came from the wallet the upload
/// named, so a copied payment from anyone else starts nothing.
async fn admit(service: &Arc<Service>, index: usize, from: u64) -> Result<u64> {
    let on = &service.chains[index];
    let head = on.client.block_number().await?;
    if head < from {
        return Ok(from);
    }
    let logs = on
        .client
        .logs(
            &Filter::new()
                .address(on.deployments.publishing)
                .event_signature(IPublishing::Submitted::SIGNATURE_HASH)
                .from_block(from)
                .to_block(head),
        )
        .await?;
    if logs.is_empty() {
        return Ok(head + 1);
    }
    let mut jobs = service.jobs.lock().await;
    for log in logs {
        let Ok(paid) = log.log_decode::<IPublishing::Submitted>() else {
            continue;
        };
        let paid = paid.inner.data;
        let Some(entry) = jobs.iter_mut().find(|entry| {
            entry.upload == paid.upload
                && entry.author == paid.author
                && entry.job.chain == on.vault_chain
                && entry.job.state == JobState::AwaitingPayment
        }) else {
            continue;
        };
        let id: u64 = paid
            .strategyId
            .try_into()
            .context("a strategy id past u64")?;
        entry.job.strategy_id = Some(id);
        entry.job.state = JobState::Queued { position: 0 };
        tracing::info!(job = entry.job.id, id, "paid for, and queued");
    }
    write_queue(service, &jobs);
    Ok(head + 1)
}

async fn list_jobs(State(service): State<Arc<Service>>) -> Json<Vec<Job>> {
    let jobs = service.jobs.lock().await;
    Json(placed(&jobs))
}

async fn one_job(
    State(service): State<Arc<Service>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Job>, StatusCode> {
    let jobs = service.jobs.lock().await;
    placed(&jobs)
        .into_iter()
        .find(|job| job.id == id)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
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

/// Take the oldest paid job, have it reviewed, and build it.
async fn take_one(service: &Arc<Service>) -> Result<()> {
    let next = {
        let jobs = service.jobs.lock().await;
        // First come, first served, and one job in flight per author: nobody
        // can fill the queue with their own work.
        let busy: Vec<Address> = jobs
            .iter()
            .filter(|entry| matches!(entry.job.state, JobState::Reviewing | JobState::Building))
            .map(|entry| entry.author)
            .collect();
        jobs.iter()
            .find(|entry| {
                matches!(entry.job.state, JobState::Queued { .. }) && !busy.contains(&entry.author)
            })
            .cloned()
    };
    let Some(entry) = next else {
        return Ok(());
    };
    let started = std::time::Instant::now();
    match run(service, &entry).await {
        Ok((state, bundle_hash)) => {
            set_state(service, &entry.job.id, state, bundle_hash, started).await
        }
        Err(error) => {
            // The fee is spent all the same: a free retry is something anyone
            // could trigger.
            tracing::warn!(id = entry.job.id, %error, "a job failed on our side");
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

    // The review comes first: nothing is compiled, so no build script or
    // procedural macro runs, before the reviewer has read the source.
    set_state(service, &entry.job.id, JobState::Reviewing, None, started).await;
    let answer: ReviewAnswer = service
        .http
        .post(format!("{}/{chain}/reviews", service.reviewer))
        .json(&ReviewRequest {
            strategy_id,
            archive_base64: entry.archive.clone(),
            sealed: entry.job.sealed,
            manifest_path: entry.manifest_path.clone(),
        })
        .timeout(Duration::from_secs(30 * 60))
        .send()
        .await
        .context("the reviewer did not answer")?
        .error_for_status()
        .context("the reviewer could not review it")?
        .json()
        .await
        .context("the reviewer's answer")?;
    let Some(clearance) = answer.clearance else {
        tracing::info!(id = entry.job.id, "the reviewer refused it");
        return Ok((JobState::Refused, None));
    };
    anyhow::ensure!(
        answer.source_hash == entry.job.source_hash,
        "the reviewer read different source"
    );

    set_state(service, &entry.job.id, JobState::Building, None, started).await;
    let source = service.builds.join(&entry.job.id).join("source");
    let _ = std::fs::remove_dir_all(source.parent().unwrap_or(&source));
    let archive = shared::base64::decode(&entry.archive).map_err(|error| anyhow!(error))?;
    shared::archive::unpack(&archive, &source)?;

    let sdk = service.sdk.clone();
    let package = entry.package.clone();
    let built = tokio::task::spawn_blocking(move || build::build(&source, &sdk, &package)).await?;
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

    let bundle_hash = built.bundle_hash.clone();
    register(service, entry, strategy_id, &archive, built, &clearance).await?;
    Ok((JobState::Ready, Some(bundle_hash)))
}

/// Write what was built into the store, then put it on the registry with the
/// reviewer's go-ahead and the builder's own signature.
async fn register(
    service: &Arc<Service>,
    entry: &Entry,
    strategy_id: u64,
    archive: &[u8],
    built: build::Built,
    clearance: &str,
) -> Result<()> {
    let chain = entry.job.chain;
    let on = service.on(chain)?;
    let source = service.builds.join(&entry.job.id).join("source");
    let manifest =
        build::read_manifest(&source.join(&entry.manifest_path)).map_err(|error| anyhow!(error))?;

    let canonical = Canonical {
        strategy_id,
        vault_chain: chain,
        author: format!("{:#x}", entry.author),
        bundle_hash: built.bundle_hash.clone(),
        component_hash: built.component_hash.clone(),
        source_hash: entry.job.source_hash.clone(),
        sealed: entry.job.sealed,
        world: build::WORLD.into(),
        toolchain: build::TOOLCHAIN.into(),
        builder_image: format!("{:#x}", chain::attest::image_digest("builder")),
        manifest: manifest.clone(),
    };
    let manifest_hash = canonical.hash();

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
    let digest = on
        .client
        .read(
            on.deployments.strategies,
            IStrategyRegistry::registrationDigestCall {
                strategyId: alloy::primitives::U256::from(strategy_id),
                author: entry.author,
                sourceHash: source_hash,
                bundleHash: bundle_hash,
                manifestHash: manifest_bytes,
                chains: chains.clone(),
                bridges: bridges.clone(),
                restricted: restricted.clone(),
            },
        )
        .await?;
    let signature = on.client.sign(on.deployments.roles.builder, digest).await?;

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
    store::put_json(&service.store, &manifest_hash, "manifest.json", &canonical)?;
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

    let clearance: Bytes = clearance.parse().context("the reviewer's go-ahead")?;
    on.client
        .send(
            on.deployments.roles.builder,
            on.deployments.strategies,
            IStrategyRegistry::registerCall {
                strategyId: alloy::primitives::U256::from(strategy_id),
                author: entry.author,
                sourceHash: source_hash,
                bundleHash: bundle_hash,
                manifestHash: manifest_bytes,
                chains,
                bridges,
                restricted,
                builderSignature: signature,
                reviewerClearance: clearance,
            },
        )
        .await
        .context("the registry would not take the registration")?;

    tracing::info!(
        id = entry.job.id,
        strategy_id,
        "built, signed and registered"
    );
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
    let mut jobs = service.jobs.lock().await;
    if let Some(entry) = jobs.iter_mut().find(|entry| entry.job.id == id) {
        if state.is_finished() {
            entry.job.duration_seconds = started.elapsed().as_secs() as u32;
        }
        entry.job.state = state;
        if let Some(hash) = bundle_hash {
            entry.job.bundle_hash = Some(hash);
        }
    }
    write_queue(service, &jobs);
}

/// Keep the queue on disk. A job that cannot be written down is still built;
/// the note about it is what would be lost.
fn write_queue(service: &Service, jobs: &[Entry]) {
    if let Some(parent) = service.queue_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_vec(jobs) {
        Ok(bytes) => {
            if let Err(error) = std::fs::write(&service.queue_file, bytes) {
                tracing::warn!(%error, "the queue could not be written down");
            }
        }
        Err(error) => tracing::warn!(%error, "the queue could not be written down"),
    }
}

fn now() -> time::PrimitiveDateTime {
    let now = time::OffsetDateTime::now_utc();
    time::PrimitiveDateTime::new(now.date(), now.time())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_becomes_the_bytes_the_chain_holds() {
        let bytes = name("local-bridge");
        assert_eq!(&bytes[..12], b"local-bridge");
        assert_eq!(bytes[12], 0);
    }
}
