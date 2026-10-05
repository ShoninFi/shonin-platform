//! The reviewer.
//!
//! The build service hands it each paid submission before anything is built.
//! It reads the source and the author's description with a pinned model, and
//! rates the code in six areas from a battery of narrow questions. Code found
//! to be malicious is refused, and the refusal is recorded on the chain;
//! anything else gets the reviewer's signed go-ahead, without which the
//! registry takes no build, and its review is posted once the build is
//! registered. The prompt, the schema and the battery are in this repository,
//! so an author can run the same review over their own code first.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{B256, U256, keccak256};
use anyhow::{Context, Result, anyhow, bail};
use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use chain::bindings::reviews::{IReviewRegistry, Review as SignedReview};
use chain::bindings::strategies::IStrategyRegistry;
use chain::{NetworkName, Served};
use clap::Parser;
use serde::{Deserialize, Serialize};
use shared::manifest::{Manifest, hash_bytes};
use shared::model::review::{Finding, Rating, Report};
use shared::{Chain, ReviewAnswer, ReviewRequest};
use shonin_reviewer::battery::{Answer, Battery};
use shonin_reviewer::{jev, source};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

/// The prompt and the schema this image is pinned to. Changing either would
/// be an image change, behind the same timelock as any other.
const PROMPT: &str = include_str!("../review/prompt.md");
const SCHEMA: &str = include_str!("../review/schema.json");

#[derive(Parser)]
#[command(about = "Reads and rates an agent's code before it is built")]
struct Args {
    #[command(flatten)]
    chain_args: chain::ChainArgs,
    #[arg(long)]
    store: PathBuf,
    /// The model this image is pinned to. Without it, on a local chain or a
    /// testnet, the reviewer lets everything through and rates every area
    /// "couldn't tell", saying why; on mainnet it refuses to start.
    #[arg(long, env = "SHONIN_REVIEW_MODEL")]
    model: Option<String>,
    /// The key for the model that answers the battery. Without it the pinned
    /// model answers the battery itself.
    #[arg(long, env = "SHONIN_CALIBRATOR_KEY")]
    calibrator_key: Option<String>,
    #[arg(long, env = "SHONIN_CALIBRATOR_URL")]
    calibrator_url: Option<String>,
    #[arg(long, env = "SHONIN_CALIBRATOR_MODEL")]
    calibrator_model: Option<String>,
    #[arg(long, default_value_t = 19_086)]
    port: u16,
}

/// What the pinned model is asked to return.
#[derive(Clone, Serialize, Deserialize)]
struct Prose {
    summary: String,
    findings: Vec<ProseFinding>,
    limits: Vec<String>,
    answers: Vec<Answer>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ProseFinding {
    area: shared::Area,
    severity: shared::Severity,
    summary: String,
    detail: String,
    file: String,
    line: u32,
}

/// A review the reviewer gave its go-ahead for, waiting for the build to be
/// registered so it can be posted. Kept on disk, so a restart posts it all
/// the same.
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    chain: Chain,
    strategy_id: u64,
    source_hash: B256,
    report_hash: B256,
    model_id_hash: B256,
    ratings: [u8; 6],
    finding_count: u16,
}

struct Reviewer {
    /// Every vault chain it reviews for, connected yet or not.
    configured: Vec<Chain>,
    /// Those of them connected and blessed on so far (see [`take_on`]). A
    /// review asked for on one not yet connected is answered 503, which the
    /// build service takes for "ask again later".
    connected: std::sync::RwLock<BTreeMap<Chain, Arc<Served>>>,
    store: PathBuf,
    model: Option<String>,
    calibrator: Option<jev::Client>,
    battery: Battery,
    pending: Mutex<Vec<Pending>>,
    pending_file: PathBuf,
}

/// Why a chain can't be acted on.
#[derive(Debug, PartialEq, Eq)]
enum Unserved {
    /// It is not one this reviewer reviews for.
    NotServed,
    /// It is, and is not connected yet.
    NotConnected,
}

impl std::fmt::Display for Unserved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotServed => "this reviewer does not serve that chain",
            Self::NotConnected => "that chain is not connected yet",
        })
    }
}

impl std::error::Error for Unserved {}

impl Reviewer {
    fn on(&self, chain: Chain) -> Result<Arc<Served>, Unserved> {
        if !self.configured.contains(&chain) {
            return Err(Unserved::NotServed);
        }
        self.connected
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&chain)
            .cloned()
            .ok_or(Unserved::NotConnected)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(chain::log_filter())
        .with_target(false)
        .init();

    let args = Args::parse();
    if args.model.is_none() && args.chain_args.network == NetworkName::Mainnet {
        bail!("a review model is required on mainnet: set SHONIN_REVIEW_MODEL");
    }
    let configured = args
        .chain_args
        .targets()?
        .iter()
        .map(|target| target.vault_chain)
        .collect();
    // Each chain is taken on as soon as it connects: one out of reach (its
    // node down, its deployment missing) is tried again in the background,
    // and keeps the reviewer from no other.
    let mut connecting = args.chain_args.connect_each()?;

    let calibrator = jev::Client::configured(
        args.calibrator_key.clone(),
        args.calibrator_url.clone(),
        args.calibrator_model.clone(),
    );
    match (&args.model, &calibrator) {
        (Some(model), Some(client)) => {
            tracing::info!(model, calibrator = client.model(), "the reviewer is up")
        }
        (Some(model), None) => tracing::info!(
            model,
            "the reviewer is up; the model answers the battery itself"
        ),
        (None, _) => tracing::warn!(
            "no review model: every area will read couldn't tell, and the report says why"
        ),
    }

    let pending_file = args.store.join("pending-reviews.json");
    let pending: Vec<Pending> = std::fs::read(&pending_file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let reviewer = Arc::new(Reviewer {
        configured,
        connected: Default::default(),
        store: args.store,
        model: args.model,
        calibrator,
        battery: Battery::pinned(),
        pending: Mutex::new(pending),
        pending_file,
    });

    let joining = reviewer.clone();
    tokio::spawn(async move {
        while let Some(served) = connecting.recv().await {
            tokio::spawn(take_on(joining.clone(), served));
        }
    });

    let poster = reviewer.clone();
    tokio::spawn(async move {
        loop {
            post_pending(&poster).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    let app = axum::Router::new()
        .route("/health", get(|| async { "reviewer" }))
        .route("/{chain}/reviews", post(review))
        .with_state(reviewer);
    let address = SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "the reviewer is open");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Take a chain on once it is connected: get the key blessed there, since
/// each chain's registry decides for itself which keys may post a review
/// there, and only then review for it. Blessing is tried again until it is
/// done, waiting longer each time.
async fn take_on(reviewer: Arc<Reviewer>, served: Served) {
    let chain = served.vault_chain;
    let key = served.deployments.roles.reviewer;
    let mut wait = Duration::from_secs(5);
    while let Err(error) =
        chain::attest::bless_service(&served.client, &served.deployments, "reviewer", key).await
    {
        tracing::warn!(%chain, error = format!("{error:#}"), seconds = wait.as_secs(), "the key could not be blessed; trying again");
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(Duration::from_secs(300));
    }
    chain::attest::keep_blessed(
        served.client.clone(),
        served.deployments.clone(),
        "reviewer",
        key,
        chain::attest::Sender::Unlocked(key),
        alloy::primitives::Address::ZERO,
        B256::ZERO,
        || {},
    );
    reviewer
        .connected
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(chain, Arc::new(served));
    tracing::info!(%chain, "reviewing on this chain");
}

async fn review(
    State(reviewer): State<Arc<Reviewer>>,
    UrlPath(chain): UrlPath<Chain>,
    Json(request): Json<ReviewRequest>,
) -> Result<Json<ReviewAnswer>, (StatusCode, String)> {
    // Asked about a chain not connected yet, the build service puts the job
    // back in its queue and asks again later.
    match reviewer.on(chain) {
        Ok(_) => {}
        Err(Unserved::NotConnected) => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                format!("{chain} is not connected yet: ask again shortly"),
            ));
        }
        Err(Unserved::NotServed) => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("this reviewer does not serve {chain}"),
            ));
        }
    }
    review_one(&reviewer, chain, request)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "a review could not be made");
            (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
        })
}

/// Read one submission and answer: a go-ahead, or a refusal on the chain.
async fn review_one(
    reviewer: &Reviewer,
    chain: Chain,
    request: ReviewRequest,
) -> Result<ReviewAnswer> {
    let on = reviewer.on(chain)?;
    let archive =
        shared::base64::decode(&request.archive_base64).map_err(|error| anyhow!(error))?;
    // The reviewer hashes what it received itself, and signs for that.
    let source_hash = hash_bytes(&archive);
    let manifest = manifest_in(&archive, &request.manifest_path)?;
    let source = source::from_archive(&archive)?;
    let dependencies = shared::dependencies::listed(&archive);

    let (prose, answers, answered_by) = match &reviewer.model {
        Some(model) => {
            let prose = ask(
                model,
                &reviewer.battery,
                &manifest,
                &dependencies,
                &source,
                request.sealed,
            )
            .await?;
            // The battery's answers come from the calibrator when there is
            // one, and from the pinned model otherwise.
            let (answered_by, answers) = match &reviewer.calibrator {
                Some(client) => match client.ask(&reviewer.battery, &source).await {
                    Ok((calibrator, answers)) => (format!("{model}+{calibrator}"), answers),
                    Err(error) => {
                        tracing::warn!(%error, "the battery went unanswered; the model's own answers stand");
                        (model.clone(), prose.answers.clone())
                    }
                },
                None => (model.clone(), prose.answers.clone()),
            };
            (prose, answers, answered_by)
        }
        None => (unread(), Vec::new(), "none".to_string()),
    };
    let (ratings, refused) = reviewer.battery.rate(&answers);

    let findings: Vec<Finding> = prose
        .findings
        .into_iter()
        .take(50)
        .map(|finding| Finding {
            area: finding.area,
            severity: finding.severity,
            summary: finding.summary,
            detail: finding.detail,
            // Sealed source is reviewed all the same; its report just never
            // says where in the code anything is.
            file: (!request.sealed && !finding.file.is_empty()).then_some(finding.file),
            line: (!request.sealed && finding.line != 0).then_some(finding.line),
        })
        .collect();
    let mut limits = prose.limits;
    if limits.is_empty() {
        limits.push("A review is one reading of the code, not a proof about it.".to_string());
    }
    let report = Report {
        ratings,
        refused,
        sealed: request.sealed,
        summary: prose.summary,
        findings,
        limits,
        answered_by: answered_by.clone(),
        answers: serde_json::to_value(&answers)?,
    };
    let body = serde_json::to_vec_pretty(&report)?;
    let report_hash = hash_bytes(&body);
    write(&reviewer.store, &report_hash, "report.json", &body)?;

    let source_bytes = to_b256(&source_hash)?;
    let report_bytes = to_b256(&report_hash)?;
    let model_id_hash = keccak256(answered_by.as_bytes());
    let codes = ratings.map(Rating::code);
    let finding_count = report.findings.len() as u16;

    if refused {
        on.client
            .send(
                on.deployments.roles.reviewer,
                on.deployments.reviews,
                IReviewRegistry::submitRefusalCall {
                    strategyId: U256::from(request.strategy_id),
                    review: signed(
                        B256::ZERO,
                        source_bytes,
                        report_bytes,
                        model_id_hash,
                        codes,
                        finding_count,
                    ),
                },
            )
            .await
            .context("the registry would not record the refusal")?;
        tracing::info!(%chain, id = request.strategy_id, "refused as malicious");
        return Ok(ReviewAnswer {
            source_hash,
            report_hash,
            clearance: None,
        });
    }

    let clearance = clearance(
        &on.client,
        on.deployments.strategies,
        on.deployments.roles.reviewer,
        request.strategy_id,
        source_bytes,
    )
    .await?;

    let mut pending = reviewer.pending.lock().await;
    pending.push(Pending {
        chain,
        strategy_id: request.strategy_id,
        source_hash: source_bytes,
        report_hash: report_bytes,
        model_id_hash,
        ratings: codes,
        finding_count,
    });
    write_pending(reviewer, &pending);
    tracing::info!(%chain, id = request.strategy_id, "read, and cleared to build");
    Ok(ReviewAnswer {
        source_hash,
        report_hash,
        clearance: Some(format!("{clearance}")),
    })
}

/// Post every review whose build has been registered since it was cleared.
/// Each is tried on its own: one whose chain can't be read, isn't connected
/// yet, or whose post the registry would not take is said and left waiting,
/// to be tried again next time, and holds up none after it.
async fn post_pending(reviewer: &Reviewer) {
    let waiting = reviewer.pending.lock().await.clone();
    let mut done = Vec::new();
    for (index, review) in waiting.iter().enumerate() {
        match post_one(reviewer, review).await {
            Ok(true) => done.push(index),
            Ok(false) => {}
            Err(error) if error.downcast_ref() == Some(&Unserved::NotConnected) => {
                tracing::debug!(chain = %review.chain, id = review.strategy_id, "a review waits for its chain to connect");
            }
            Err(error) => {
                tracing::warn!(chain = %review.chain, id = review.strategy_id, error = format!("{error:#}"), "could not post a review; trying again later");
            }
        }
    }
    if !done.is_empty() {
        let mut pending = reviewer.pending.lock().await;
        let posted: Vec<(Chain, u64)> = done
            .iter()
            .map(|&i| (waiting[i].chain, waiting[i].strategy_id))
            .collect();
        pending.retain(|review| !posted.contains(&(review.chain, review.strategy_id)));
        write_pending(reviewer, &pending);
    }
}

/// Post one review if its build has been registered: whether it is done
/// with (posted now, or already), or still waits for its build.
async fn post_one(reviewer: &Reviewer, review: &Pending) -> Result<bool> {
    let on = reviewer.on(review.chain)?;
    let id = U256::from(review.strategy_id);
    let reviews = on.deployments.reviews;
    let already = on
        .client
        .read(reviews, IReviewRegistry::reviewOfCall { strategyId: id })
        .await?;
    if already.review.reviewedAt != 0 {
        return Ok(true);
    }
    let bundle = on
        .client
        .read(
            on.deployments.strategies,
            IStrategyRegistry::bundleHashOfCall { strategyId: id },
        )
        .await?;
    // Not built yet, or never will be: a failed build has nothing to post
    // against, and its review stays in the store.
    if bundle == B256::ZERO {
        return Ok(false);
    }
    on.client
        .send(
            on.deployments.roles.reviewer,
            reviews,
            IReviewRegistry::submitReviewCall {
                strategyId: id,
                review: signed(
                    bundle,
                    review.source_hash,
                    review.report_hash,
                    review.model_id_hash,
                    review.ratings,
                    review.finding_count,
                ),
            },
        )
        .await
        .context("the registry would not take the review")?;
    tracing::info!(chain = %review.chain, id = review.strategy_id, "review posted");
    Ok(true)
}

fn signed(
    bundle: B256,
    source: B256,
    report: B256,
    model: B256,
    ratings: [u8; 6],
    findings: u16,
) -> SignedReview {
    SignedReview {
        bundleHash: bundle,
        sourceHash: source,
        // The registry records the image it knows the reviewer key by, and
        // the time it received this; what is sent here is only a courtesy.
        reviewerImage: chain::attest::image_digest("reviewer"),
        modelIdHash: model,
        // What the model was asked, the shape it had to answer in, and the
        // questions the ratings come from, together.
        promptHash: keccak256(
            [
                PROMPT.as_bytes(),
                SCHEMA.as_bytes(),
                shonin_reviewer::battery::BATTERY.as_bytes(),
            ]
            .concat(),
        ),
        reportHash: report,
        ratings,
        findingCount: findings,
        reviewedAt: 0,
    }
}

/// The crates.io crates a project locks, as the model is told of them.
fn dependencies_named(dependencies: &[(String, String)]) -> String {
    if dependencies.is_empty() {
        return "It depends on no crates.io crate.".into();
    }
    format!(
        "It depends on these crates.io crates, which are not in the source below: each is on \
         the platform's list of crates it reviewed itself, by version and checksum, and a \
         project may depend on no others. Crates inside the project are in the source, and \
         shonin-strategy-sdk is the platform's own SDK.\n{}",
        dependencies
            .iter()
            .map(|(name, version)| format!("- {name} {version}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// What a review says when there is no model to read with.
fn unread() -> Prose {
    Prose {
        summary: "No review model is configured on this network, so the code was not read.".into(),
        findings: Vec::new(),
        limits: vec!["Nothing was read: every area reads couldn't tell.".into()],
        answers: Vec::new(),
    }
}

/// Run the pinned model over the description and the source, with the schema
/// it must answer in. The crates.io crates the project locks are named, not
/// shown: the platform reviewed them itself, and a project may depend on no
/// others (see [`shared::dependencies`]).
async fn ask(
    model: &str,
    battery: &Battery,
    manifest: &Manifest,
    dependencies: &[(String, String)],
    source: &str,
    sealed: bool,
) -> Result<Prose> {
    let description = format!(
        "Title: {}\n\n{}\n\nClaims:\n{}\n\nThe source is {}.\n\n{}",
        manifest.description.title,
        manifest.description.text,
        manifest
            .description
            .claims
            .iter()
            .map(|claim| format!("- {claim}"))
            .collect::<Vec<_>>()
            .join("\n"),
        if sealed { "sealed" } else { "published" },
        dependencies_named(dependencies),
    );
    let prompt = PROMPT
        .replace("{battery}", &battery.as_prompt())
        .replace("{description}", &description);

    let mut child = tokio::process::Command::new("claude")
        .args([
            "-p",
            "--output-format",
            "json",
            "--json-schema",
            SCHEMA,
            "--model",
            model,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("the reviewer's model could not be started")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(format!("{prompt}\n\n{source}\n").as_bytes())
            .await?;
        stdin.shutdown().await?;
    }
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        bail!("the reviewer's model did not answer");
    }
    let reply: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("the reviewer's model said something unexpected")?;
    let structured = reply
        .get("structured_output")
        .cloned()
        .context("the reviewer's model answered outside its schema")?;
    serde_json::from_value(structured).context("the report did not match the schema")
}

/// The reviewer's go-ahead for a source: its signature over the registry's
/// clearance digest, worked out here as the registry writes it and never read
/// from the node, which could otherwise hand over the digest of anything the
/// reviewer's signature counts for and have it signed. The chain id is the
/// configured one, checked against the node when it connected.
async fn clearance(
    client: &chain::Client,
    registry: alloy::primitives::Address,
    reviewer: alloy::primitives::Address,
    strategy_id: u64,
    source_hash: B256,
) -> Result<alloy::primitives::Bytes> {
    let digest = chain::digest::clearance(
        client.chain_id,
        registry,
        U256::from(strategy_id),
        source_hash,
    );
    client.sign(reviewer, digest).await
}

/// The manifest at `path` inside a packed project: `./shonin.toml` and
/// `shonin.toml` are the same file, as the builder reads them.
fn manifest_in(archive: &[u8], path: &str) -> Result<Manifest> {
    let wanted = parts(Path::new(path));
    let mut reader = tar::Archive::new(std::io::Cursor::new(archive));
    for entry in reader.entries()?.flatten() {
        let mut entry = entry;
        if entry.header().entry_type() == tar::EntryType::Regular && parts(&entry.path()?) == wanted
        {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut entry, &mut text)?;
            return Manifest::from_toml(&text).map_err(|error| anyhow!(error));
        }
    }
    bail!("no {path} in the project")
}

/// A path's named parts, so `./a/b` and `a/b` are the same path.
fn parts(path: &Path) -> Vec<std::ffi::OsString> {
    path.components()
        .filter_map(|part| match part {
            std::path::Component::Normal(name) => Some(name.to_os_string()),
            _ => None,
        })
        .collect()
}

fn write(store: &Path, hash: &str, name: &str, bytes: &[u8]) -> Result<()> {
    let directory = store.join(hash.replace(':', "-"));
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join(name), bytes)?;
    Ok(())
}

fn write_pending(reviewer: &Reviewer, pending: &[Pending]) {
    if let Some(parent) = reviewer.pending_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(pending)
        && let Err(error) = std::fs::write(&reviewer.pending_file, bytes)
    {
        tracing::warn!(%error, "the reviews waiting to be posted could not be written down");
    }
}

fn to_b256(hash: &str) -> Result<B256> {
    let bytes = shared::manifest::hash_to_bytes(hash).context("a hash")?;
    Ok(B256::from(bytes))
}

/// Keep the areas and severities the schema names in step with the model the
/// rest of the platform reads, and every battery question in the prompt.
#[cfg(test)]
mod tests {
    use super::*;
    use shared::{Area, Severity};

    fn schema() -> serde_json::Value {
        serde_json::from_str(SCHEMA).expect("the schema parses")
    }

    #[test]
    fn every_area_the_schema_names_is_one_the_platform_knows() {
        let names = schema()["properties"]["findings"]["items"]["properties"]["area"]["enum"]
            .as_array()
            .expect("the areas are listed")
            .clone();
        assert_eq!(names.len(), Area::ALL.len());
        for name in names {
            serde_json::from_value::<Area>(name).expect("the platform knows it");
        }
    }

    #[test]
    fn every_severity_the_schema_names_is_one_the_platform_knows() {
        let names = schema()["properties"]["findings"]["items"]["properties"]["severity"]["enum"]
            .as_array()
            .expect("the severities are listed")
            .clone();
        for name in names {
            serde_json::from_value::<Severity>(name).expect("the platform knows it");
        }
    }

    #[test]
    fn the_prompt_carries_the_battery_and_the_description() {
        assert!(PROMPT.contains("{battery}") && PROMPT.contains("{description}"));
        let listed = Battery::pinned().as_prompt();
        for question in Battery::pinned().questions {
            assert!(listed.contains(&question.id));
        }
    }

    /// A packed project holding `shonin.toml` at its root.
    fn packed(path: &str) -> Vec<u8> {
        let text = "[vault]\nchain = \"base\"\n[description]\ntitle = \"t\"\ntext = \"x\"\n";
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(text.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, path, text.as_bytes())
            .unwrap();
        builder.into_inner().unwrap()
    }

    #[test]
    fn a_manifest_named_from_the_project_s_root_is_read_however_it_is_spelt() {
        for packed_as in ["shonin.toml", "./shonin.toml"] {
            for asked_as in ["shonin.toml", "./shonin.toml"] {
                let manifest = manifest_in(&packed(packed_as), asked_as);
                assert!(
                    manifest.is_ok(),
                    "{asked_as} was not found in a project packing {packed_as}: {manifest:?}"
                );
            }
        }
    }

    /// The go-ahead is signed over the clearance digest the registry checks,
    /// whatever the node says that digest is.
    #[tokio::test]
    async fn the_go_ahead_is_signed_under_a_digest_the_reviewer_works_out_itself() {
        use alloy::signers::local::PrivateKeySigner;
        use alloy::sol_types::SolCall;

        // A node that answers every `clearanceDigest` with the digest of
        // another strategy's source.
        let lie = keccak256("another strategy's source");
        let app = axum::Router::new().route(
            "/",
            post(move |Json(request): Json<serde_json::Value>| async move {
                let result = match request["method"].as_str().unwrap_or_default() {
                    "eth_chainId" => serde_json::json!("0x7a69"),
                    "eth_call" => serde_json::json!(alloy::hex::encode_prefixed(
                        IStrategyRegistry::clearanceDigestCall::abi_encode_returns(&lie)
                    )),
                    _ => serde_json::Value::Null,
                };
                Json(serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": result}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let key = PrivateKeySigner::random();
        let reviewer = key.address();
        let client = chain::Client::connect(
            &chain::ChainConfig {
                label: "stand-in".to_string(),
                rpc: url.clone(),
                ws: None,
                dollar: "USDC".to_string(),
                native_usd: None,
                public_rpc: url,
                confirmations: None,
            },
            chain::Signing::Keys(vec![key]),
        )
        .await
        .unwrap();

        let registry = alloy::primitives::Address::repeat_byte(0x22);
        let source = B256::repeat_byte(0xab);
        let signature = clearance(&client, registry, reviewer, 7, source)
            .await
            .unwrap();
        let signature = alloy::primitives::Signature::try_from(signature.as_ref()).unwrap();
        let signer_of = |digest: B256| signature.recover_address_from_msg(digest).unwrap();
        assert_ne!(
            signer_of(lie),
            reviewer,
            "the reviewer signed the digest the node handed it"
        );
        assert_eq!(
            signer_of(chain::digest::clearance(
                31_337,
                registry,
                U256::from(7u64),
                source
            )),
            reviewer,
            "the go-ahead is not over the registry's clearance digest of this source"
        );
    }

    /// A stand-in node answering as chain `chain_id`: `eth_call` with what
    /// `call` makes of the request (`None` for a refusal), and every other
    /// method but `eth_chainId` with a refusal. Every call's input is noted
    /// in `asked`. Where it is.
    async fn a_node(
        chain_id: u64,
        call: fn(&[u8]) -> Option<String>,
        asked: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    ) -> String {
        let app = axum::Router::new().route(
            "/",
            post(move |Json(request): Json<serde_json::Value>| {
                let asked = asked.clone();
                async move {
                    let id = request["id"].clone();
                    let answer = match request["method"].as_str().unwrap_or_default() {
                        "eth_chainId" => Some(format!("0x{chain_id:x}")),
                        "eth_call" => {
                            let tx = &request["params"][0];
                            let input = alloy::hex::decode(
                                tx["input"].as_str().or(tx["data"].as_str()).unwrap_or("0x"),
                            )
                            .unwrap_or_default();
                            asked.lock().unwrap().push(input.clone());
                            call(&input)
                        }
                        _ => None,
                    };
                    Json(match answer {
                        Some(result) => {
                            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
                        }
                        None => serde_json::json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": {"code": 3, "message": "execution reverted"},
                        }),
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    /// The chain at `url` as a reviewer serves it, as `vault_chain`.
    async fn served(url: &str, vault_chain: Chain, chain_id: u64) -> Served {
        let client = chain::Client::connect(
            &chain::ChainConfig {
                label: "stand-in".to_string(),
                rpc: url.to_string(),
                ws: None,
                dollar: "USDC".to_string(),
                native_usd: None,
                public_rpc: url.to_string(),
                confirmations: None,
            },
            chain::Signing::ReadOnly,
        )
        .await
        .unwrap();
        let one = "0x0000000000000000000000000000000000000001";
        let mut fields = vec![
            format!(r#""chain_key":"evm:{chain_id}""#),
            format!(r#""chain_id":{chain_id}"#),
            r#""rpc_url":"""#.to_string(),
            r#""deployed_block":0"#.to_string(),
        ];
        for name in [
            "usd",
            "venue",
            "attestation_verifier",
            "attestation",
            "publishing",
            "prices",
            "strategies",
            "reviews",
            "profiles",
            "factory",
            "station_factory",
            "driver",
            "bridge",
        ] {
            fields.push(format!(r#""{name}":"{one}""#));
        }
        let roles: Vec<String> = [
            "admin", "platform", "attestor", "builder", "reviewer", "executor", "keeper",
            "operator", "author", "viewer",
        ]
        .iter()
        .map(|role| format!(r#""{role}":"{one}""#))
        .collect();
        fields.push(format!(r#""roles":{{{}}}"#, roles.join(",")));
        Served {
            vault_chain,
            key: format!("evm:{chain_id}").parse().unwrap(),
            client,
            deployments: chain::Deployments::from_json(&format!("{{{}}}", fields.join(",")))
                .unwrap(),
            native_usd: None,
            public_rpc: String::new(),
        }
    }

    fn waiting(chain: Chain, strategy_id: u64) -> Pending {
        Pending {
            chain,
            strategy_id,
            source_hash: B256::repeat_byte(1),
            report_hash: B256::repeat_byte(2),
            model_id_hash: B256::repeat_byte(3),
            ratings: [1; 6],
            finding_count: 0,
        }
    }

    /// One pending review whose chain can't be read, and one whose post the
    /// registry would not take, hold up none after them: a later chain's
    /// review is still looked at, and is done with, and the two are kept to
    /// be tried again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_review_that_cannot_be_posted_holds_up_none_after_it() {
        use alloy::sol_types::SolCall;

        // Base: strategy 1's review can't be read; strategy 2's build is
        // registered and its post is refused.
        let base_asked: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Default::default();
        let base = a_node(
            31_337,
            |input| {
                let selector: [u8; 4] = input.get(..4)?.try_into().ok()?;
                let id = U256::from_be_slice(input.get(4..36)?);
                match selector {
                    IReviewRegistry::reviewOfCall::SELECTOR if id == U256::from(1) => None,
                    // Never reviewed: every word zero.
                    IReviewRegistry::reviewOfCall::SELECTOR => {
                        Some(format!("0x{}", "0".repeat(64 * 40)))
                    }
                    IStrategyRegistry::bundleHashOfCall::SELECTOR => {
                        Some(format!("0x{}", "42".repeat(32)))
                    }
                    _ => None,
                }
            },
            base_asked.clone(),
        )
        .await;
        // Robinhood: strategy 3's review is posted already.
        let robinhood_asked: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Default::default();
        let robinhood = a_node(
            46_630,
            |input| {
                (input.get(..4)? == IReviewRegistry::reviewOfCall::SELECTOR)
                    .then(|| format!("0x{}", format!("{:064x}", 1).repeat(40)))
            },
            robinhood_asked.clone(),
        )
        .await;

        let store =
            std::env::temp_dir().join(format!("shonin-reviewer-pending-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&store);
        let reviewer = Reviewer {
            configured: vec![Chain::Base, Chain::Robinhood],
            connected: std::sync::RwLock::new(BTreeMap::from([
                (
                    Chain::Base,
                    Arc::new(served(&base, Chain::Base, 31_337).await),
                ),
                (
                    Chain::Robinhood,
                    Arc::new(served(&robinhood, Chain::Robinhood, 46_630).await),
                ),
            ])),
            pending_file: store.join("pending-reviews.json"),
            store,
            model: None,
            calibrator: None,
            battery: Battery::pinned(),
            pending: Mutex::new(vec![
                waiting(Chain::Base, 1),
                waiting(Chain::Base, 2),
                waiting(Chain::Robinhood, 3),
            ]),
        };

        post_pending(&reviewer).await;

        assert!(
            robinhood_asked
                .lock()
                .unwrap()
                .iter()
                .any(|input| input.starts_with(&IReviewRegistry::reviewOfCall::SELECTOR)),
            "a later chain's pending review was never looked at"
        );
        let left: Vec<(Chain, u64)> = reviewer
            .pending
            .lock()
            .await
            .iter()
            .map(|review| (review.chain, review.strategy_id))
            .collect();
        assert_eq!(
            left,
            [(Chain::Base, 1), (Chain::Base, 2)],
            "the posted review is done with, and the two that could not be are kept"
        );
        assert!(
            base_asked
                .lock()
                .unwrap()
                .iter()
                .any(|input| input.starts_with(&IStrategyRegistry::bundleHashOfCall::SELECTOR)),
            "the review after an unreadable one on the same chain was never looked at"
        );
        let _ = std::fs::remove_dir_all(&reviewer.store);
    }

    #[test]
    fn with_no_model_every_area_reads_couldnt_tell() {
        let (ratings, refused) = Battery::pinned().rate(&unread().answers);
        assert_eq!(ratings, [Rating::CouldntTell; 6]);
        assert!(!refused);
    }

    /// The model is told which reviewed crates a project depends on, by
    /// name and version, since their source is not in what it reads.
    #[test]
    fn the_model_is_told_every_crate_the_project_depends_on() {
        let told = dependencies_named(&[
            ("futures".to_string(), "0.3.34".to_string()),
            ("serde".to_string(), "1.0.229".to_string()),
        ]);
        assert!(told.contains("- futures 0.3.34\n- serde 1.0.229"), "{told}");
        assert!(dependencies_named(&[]).contains("no crates.io crate"));
    }
}
