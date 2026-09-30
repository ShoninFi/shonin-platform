//! The build service.
//!
//! An author submits a project; the service compiles it with a pinned recipe,
//! loads the component, and signs what it made with a key the registry blessed
//! for the image it is. The queue is public and
//! first come, first served, one job in flight per author, and a failed build
//! is as public as one that worked.

mod build;
mod store;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, B256, U256, keccak256};
use anyhow::{Context, Result};
use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::strategies::IStrategyRegistry;
use chain::bindings::vault::{IVault, VaultState};
use chain::{Client, Deployments, Served};
use clap::Parser;
use shared::manifest::{Manifest, hash_bytes, hash_to_bytes};
use shared::model::registry::{Job, JobState};
use shared::{Chain, Submission, SubmissionAccepted, SubmissionSource};
use tokio::sync::Mutex;

#[derive(Parser)]
#[command(about = "Compiles submitted strategies into signed bundles")]
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
    #[arg(long, default_value_t = 19_084)]
    port: u16,
}

struct Service {
    /// Every vault chain it builds for: a build is registered on the chain its
    /// vault is on.
    chains: Vec<Served>,
    store: PathBuf,
    builds: PathBuf,
    sdk: PathBuf,
    /// The queue, oldest first.
    jobs: Mutex<Vec<Entry>>,
    /// Where the queue is kept, so restarting the service does not lose a
    /// build someone paid for.
    queue_file: PathBuf,
}

impl Service {
    /// The chain a build for this vault chain is registered on.
    fn on(&self, chain: Chain) -> Result<&Served> {
        self.chains
            .iter()
            .find(|served| served.vault_chain == chain)
            .ok_or_else(|| anyhow::anyhow!("this build service does not serve {chain}"))
    }
}

/// A job, and what it was asked to build.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Entry {
    job: Job,
    submission: Submission,
    /// The submitted source, base64 as it arrived, so a restart can still
    /// build it.
    archive: String,
}

impl Entry {
    fn bytes(&self) -> Result<Vec<u8>> {
        decode(&self.archive)
    }
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
    let chains = args.chain_args.connect_all().await?;
    for served in &chains {
        bless(&served.client, &served.deployments).await?;
    }

    // A build someone funded should survive this service restarting, so the
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
        store: args.store,
        builds: args.builds,
        sdk: args.sdk,
        jobs: Mutex::new(waiting),
        queue_file,
    });

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
        .route("/{chain}/submissions", post(submit))
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

// The queue ------------------------------------------------------------------

async fn submit(
    State(service): State<Arc<Service>>,
    axum::extract::Path(chain): axum::extract::Path<Chain>,
    Json(submission): Json<Submission>,
) -> Result<Json<SubmissionAccepted>, (StatusCode, String)> {
    let archive = match &submission.source {
        SubmissionSource::Archive { base64 } => {
            decode(base64).map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?
        }
        SubmissionSource::Repository { .. } => {
            return Err((
                StatusCode::BAD_REQUEST,
                "this build service takes a packed project, not a repository".to_string(),
            ));
        }
    };
    let source_hash = hash_bytes(&archive);
    if let Err(error) = service.on(chain) {
        return Err((StatusCode::BAD_REQUEST, error.to_string()));
    }

    let mut jobs = service.jobs.lock().await;
    let id = format!("{}", 8_800 + jobs.len());
    let now = now();
    let position = jobs
        .iter()
        .filter(|entry| matches!(entry.job.state, JobState::Queued { .. }))
        .count() as u32
        + 1;

    let job = Job {
        id: id.clone(),
        chain,
        subject: format!("{} v{}", submission.strategy_id, submission.version),
        author: submission.author.clone(),
        state: JobState::Queued { position },
        submitted: now,
        duration_seconds: 0,
        source_hash: source_hash.clone(),
        bundle_hash: None,
        log: vec![build::plain(format!(
            "queued  {} v{}",
            submission.strategy_id, submission.version
        ))],
        // What this build is for, from the moment it is queued: a build that
        // fails never registers a strategy, and its page still has to be able
        // to find the log that says why.
        strategy_slug: Some(format!(
            "{}-v{}",
            submission.strategy_id, submission.version
        )),
    };
    jobs.push(Entry {
        job: job.clone(),
        submission,
        archive: encode(&archive),
    });
    write_queue(&service, &jobs);

    tracing::info!(id, "a submission joined the queue");
    Ok(Json(SubmissionAccepted {
        job_id: id,
        source_hash,
    }))
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
/// asked for, not when it was submitted, so a queue that drains is a queue
/// that visibly drains.
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

/// What the build cost, at the platform's published prices, charged to the
/// vault's tank. A build is processor time and nothing else, so that is what
/// it is billed as.
async fn charge_build(service: &Arc<Service>, chain: Chain, vault: Address, seconds: u32) {
    let Ok(on) = service.on(chain) else {
        return;
    };
    let dollars = seconds as f64 / 3_600.0 * shared::Prices::published().vcpu_hour;
    let amount = U256::from((dollars * 1_000_000.0).round().max(0.0) as u128);
    if let Err(error) = on
        .client
        .send(
            on.deployments.roles.builder,
            vault,
            IVault::chargeBuildCall { amount },
        )
        .await
    {
        // The build still happened and the vault still opens: an uncharged
        // build costs the platform, never the author or the depositors.
        tracing::warn!(%vault, %error, "the build could not be charged");
    }
}

/// Take the oldest job whose vault is ready, and build it.
async fn take_one(service: &Arc<Service>) -> Result<()> {
    let next = {
        let jobs = service.jobs.lock().await;
        // First come, first served, and one job in flight per author: nobody
        // can fill the queue with their own work.
        let busy: Vec<String> = jobs
            .iter()
            .filter(|entry| matches!(entry.job.state, JobState::Building { .. }))
            .map(|entry| entry.job.author.clone())
            .collect();
        jobs.iter()
            .find(|entry| {
                matches!(entry.job.state, JobState::Queued { .. })
                    && !busy.contains(&entry.job.author)
            })
            .cloned()
    };
    let Some(entry) = next else {
        return Ok(());
    };

    // A build is admitted once its vault exists and its tank covers the
    // minimum. Until then the job waits, and nothing was built for free.
    let chain = entry.job.chain;
    let Some(vault) = vault_for(service, chain, &entry.submission).await? else {
        return Ok(());
    };

    set_state(
        service,
        &entry.job.id,
        JobState::Building {
            step: "compiling".to_string(),
            percent: 10,
        },
        None,
    )
    .await;

    let source = service.builds.join(&entry.job.id).join("source");
    let _ = std::fs::remove_dir_all(source.parent().unwrap_or(&source));
    shared::archive::unpack(&entry.bytes()?, &source)?;

    let sdk = service.sdk.clone();
    let claimed = (
        entry.submission.strategy_id.clone(),
        entry.submission.version,
    );
    let built = tokio::task::spawn_blocking(move || {
        build::build(&source, &sdk, "sha256:dev-builder", (&claimed.0, claimed.1))
    })
    .await?;

    // A build that ran is charged, whichever way it ended. The vault's
    // contract believes the figure only up to a day at the allowances its
    // author declared.
    let ran = match &built {
        Ok(built) => built.seconds,
        Err(failed) => failed.seconds,
    };
    charge_build(service, chain, vault, ran).await;

    match built {
        Ok(built) => finish(service, chain, &entry, vault, built).await,
        Err(failed) => {
            tracing::warn!(id = entry.job.id, reason = failed.reason, "a build failed");
            set_state(
                service,
                &entry.job.id,
                JobState::Failed {
                    reason: failed.reason.clone(),
                },
                Some(failed.log),
            )
            .await;
            // The build ran, so the tank is charged for it, and the vault
            // closes with the rest returned to its author.
            let on = service.on(chain)?;
            on.client
                .send(
                    on.deployments.roles.builder,
                    vault,
                    IVault::failBuildCall {
                        reasonHash: keccak256(failed.reason.as_bytes()),
                    },
                )
                .await
                .ok();
            Ok(())
        }
    }
}

/// Sign what was built and put it on the registry, then write everything the
/// site will show into the store.
async fn finish(
    service: &Arc<Service>,
    chain: Chain,
    entry: &Entry,
    vault: Address,
    built: build::Built,
) -> Result<()> {
    let on = service.on(chain)?;
    let submission = &entry.submission;
    let author: Address = submission.author.parse().context("the author's address")?;
    let strategy_id = name_to_bytes(&submission.strategy_id);
    let source_hash = hash_to_bytes(&entry.job.source_hash).context("the source hash")?;
    let bundle_hash = hash_to_bytes(&built.bundle_hash).context("the bundle hash")?;
    let manifest_hash = hash_to_bytes(&built.manifest_hash).context("the manifest hash")?;

    let digest = on
        .client
        .read(
            on.deployments.strategies,
            IStrategyRegistry::registrationDigestCall {
                strategyId: strategy_id.into(),
                version: submission.version,
                author,
                sourceHash: source_hash.into(),
                bundleHash: bundle_hash.into(),
                manifestHash: manifest_hash.into(),
            },
        )
        .await?;
    let signature = on.client.sign(on.deployments.roles.builder, digest).await?;

    // Everything the site shows about a build lives in the store, addressed
    // by the hashes the chain holds. It is written before the registration,
    // because a registry pointing at bytes nobody has is worse than bytes
    // nobody has looked up yet: the reviewer reads the source the moment the
    // version appears.
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
    store::put_json(
        &service.store,
        &built.bundle_hash,
        "manifest.json",
        &built.manifest,
    )?;
    store::put_json(&service.store, &built.bundle_hash, "log.json", &built.log)?;
    // The hashes this build produced, beside the bytes they name, so a page
    // can show them and a reader can check them.
    store::put_json(
        &service.store,
        &built.bundle_hash,
        "hashes.json",
        &serde_json::json!({
            "component": built.component_hash,
            "bundle": built.bundle_hash,
            "manifest": built.manifest_hash,
            "source": entry.job.source_hash,
            "toolchain": built.toolchain,
            "seconds": built.seconds,
        }),
    )?;
    // The source is kept either way, because the reviewer reads it either way
    // and a rebuild has to be possible for as long as the vault lives. A
    // sealed one is kept under a name the public store refuses to serve, so
    // sealing changes who can read the source and nothing else.
    store::put(
        &service.store,
        &entry.job.source_hash,
        if submission.sealed {
            "source.sealed.tar"
        } else {
            "source.tar"
        },
        &entry.bytes()?,
    )?;

    let (supersedes_id, supersedes_version) = supersedes(&built.manifest);
    on.client
        .send(
            on.deployments.roles.builder,
            on.deployments.strategies,
            IStrategyRegistry::registerCall {
                strategyId: strategy_id.into(),
                version: submission.version,
                author,
                sourceHash: source_hash.into(),
                bundleHash: bundle_hash.into(),
                manifestHash: manifest_hash.into(),
                supersedesId: supersedes_id.into(),
                supersedesVersion: supersedes_version,
                builderSignature: signature,
            },
        )
        .await
        .context("the registry would not take the registration")?;

    // Anyone may open a vault once the registry holds its bundle.
    on.client
        .send(on.deployments.roles.builder, vault, IVault::openCall {})
        .await
        .context("the vault would not open")?;

    let slug = built.manifest.slug();
    tracing::info!(id = entry.job.id, slug, "built, signed and registered");
    set_state_full(
        service,
        &entry.job.id,
        JobState::Succeeded,
        Some(built.log),
        Some(built.bundle_hash),
        Some(slug),
        built.seconds,
    )
    .await;
    Ok(())
}

/// The vault a submission belongs to, once it exists and is still waiting for
/// its build.
async fn vault_for(
    service: &Arc<Service>,
    chain: Chain,
    submission: &Submission,
) -> Result<Option<Address>> {
    let on = service.on(chain)?;
    let vault = on
        .client
        .read(
            on.deployments.factory,
            IVaultFactory::vaultOfCall {
                strategyId: name_to_bytes(&submission.strategy_id).into(),
                version: submission.version,
            },
        )
        .await?;
    if vault == Address::ZERO {
        return Ok(None);
    }
    let state = on.client.read(vault, IVault::stateCall {}).await?;
    Ok(matches!(state, VaultState::Building).then_some(vault))
}

fn supersedes(manifest: &Manifest) -> ([u8; 32], u32) {
    let Some(claim) = &manifest.strategy.supersedes else {
        return ([0u8; 32], 0);
    };
    match claim.rsplit_once('@') {
        Some((id, version)) => (name_to_bytes(id), version.parse().unwrap_or(0)),
        None => ([0u8; 32], 0),
    }
}

async fn set_state(
    service: &Arc<Service>,
    id: &str,
    state: JobState,
    log: Option<Vec<shared::model::bundle::LogLine>>,
) {
    set_state_full(service, id, state, log, None, None, 0).await;
}

async fn set_state_full(
    service: &Arc<Service>,
    id: &str,
    state: JobState,
    log: Option<Vec<shared::model::bundle::LogLine>>,
    bundle_hash: Option<String>,
    slug: Option<String>,
    seconds: u32,
) {
    let mut jobs = service.jobs.lock().await;
    if let Some(entry) = jobs.iter_mut().find(|entry| entry.job.id == id) {
        entry.job.state = state;
        if let Some(log) = log {
            entry.job.log = log;
        }
        if let Some(hash) = bundle_hash {
            entry.job.bundle_hash = Some(hash);
        }
        if let Some(slug) = slug {
            entry.job.strategy_slug = Some(slug);
        }
        if seconds != 0 {
            entry.job.duration_seconds = seconds;
        }
    }
    write_queue(service, &jobs);
}

/// Keep the queue on disk. A build that cannot be written down is still built;
/// the note about it is what would be lost.
fn write_queue(service: &Arc<Service>, jobs: &[Entry]) {
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

/// A short name as the chain stores it: the text, then zeros.
fn name_to_bytes(name: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let bytes = name.as_bytes();
    let take = bytes.len().min(32);
    out[..take].copy_from_slice(&bytes[..take]);
    out
}

fn now() -> time::PrimitiveDateTime {
    let now = time::OffsetDateTime::now_utc();
    time::PrimitiveDateTime::new(now.date(), now.time())
}

/// Base64, without pulling in a crate for sixty lines of table lookup.
fn decode(text: &str) -> Result<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [255u8; 256];
    for (index, byte) in ALPHABET.iter().enumerate() {
        table[*byte as usize] = index as u8;
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        if byte == b'=' || byte == b'\n' || byte == b'\r' {
            continue;
        }
        let value = table[byte as usize];
        anyhow::ensure!(value != 255, "that is not an archive");
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    Ok(out)
}

/// The other direction, for whoever is packing one.
pub fn encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut buffer = [0u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let value = u32::from_be_bytes([0, buffer[0], buffer[1], buffer[2]]);
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(ALPHABET[((value >> (18 - index * 6)) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_survives_a_round_trip() {
        for body in [
            b"".to_vec(),
            b"a".to_vec(),
            b"ab".to_vec(),
            b"abc".to_vec(),
            (0u8..=255).collect::<Vec<u8>>(),
        ] {
            let text = encode(&body);
            assert_eq!(
                decode(&text).unwrap(),
                body,
                "round trip of {} bytes",
                body.len()
            );
        }
    }

    #[test]
    fn a_name_becomes_the_bytes_the_chain_holds() {
        let bytes = name_to_bytes("funding-basis");
        assert_eq!(&bytes[..13], b"funding-basis");
        assert_eq!(bytes[13], 0);
    }

    #[test]
    fn a_supersession_claim_is_read_as_a_version() {
        let manifest = Manifest::from_toml(
            r#"
[strategy]
id = "x"
version = 2
supersedes = "funding-basis@1"
[description]
title = "t"
text = "t"
[runtime.allowances]
fuel_per_sec = 1
memory_mib = 1
requests_per_min = 1
bytes_per_day = 1
streams = 1
inference_ms_per_min = 0
"#,
        )
        .unwrap();
        let (id, version) = supersedes(&manifest);
        assert_eq!(&id[..13], b"funding-basis");
        assert_eq!(version, 1);
    }
}
