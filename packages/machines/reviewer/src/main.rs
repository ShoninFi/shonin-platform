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
    chains: Vec<Served>,
    store: PathBuf,
    model: Option<String>,
    calibrator: Option<jev::Client>,
    battery: Battery,
    pending: Mutex<Vec<Pending>>,
    pending_file: PathBuf,
}

impl Reviewer {
    fn on(&self, chain: Chain) -> Result<&Served> {
        self.chains
            .iter()
            .find(|served| served.vault_chain == chain)
            .ok_or_else(|| anyhow!("this reviewer does not serve {chain}"))
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
    if args.model.is_none() && args.chain_args.network == NetworkName::Mainnet {
        bail!("a review model is required on mainnet: set SHONIN_REVIEW_MODEL");
    }
    let chains = args.chain_args.connect_all().await?;
    // Blessed on every vault chain it serves, since each chain's registry
    // decides for itself which keys may post a review there.
    for served in &chains {
        let key = served.deployments.roles.reviewer;
        chain::attest::bless_service(&served.client, &served.deployments, "reviewer", key).await?;
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
    }

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
        chains,
        store: args.store,
        model: args.model,
        calibrator,
        battery: Battery::pinned(),
        pending: Mutex::new(pending),
        pending_file,
    });

    let poster = reviewer.clone();
    tokio::spawn(async move {
        loop {
            if let Err(error) = post_pending(&poster).await {
                tracing::warn!(%error, "could not post a review");
            }
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

async fn review(
    State(reviewer): State<Arc<Reviewer>>,
    UrlPath(chain): UrlPath<Chain>,
    Json(request): Json<ReviewRequest>,
) -> Result<Json<ReviewAnswer>, (StatusCode, String)> {
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

    let (prose, answers, answered_by) = match &reviewer.model {
        Some(model) => {
            let prose = ask(model, &reviewer.battery, &manifest, &source, request.sealed).await?;
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

    let digest = on
        .client
        .read(
            on.deployments.strategies,
            IStrategyRegistry::clearanceDigestCall {
                strategyId: U256::from(request.strategy_id),
                sourceHash: source_bytes,
            },
        )
        .await?;
    let clearance = on
        .client
        .sign(on.deployments.roles.reviewer, digest)
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
async fn post_pending(reviewer: &Reviewer) -> Result<()> {
    let waiting = reviewer.pending.lock().await.clone();
    let mut done = Vec::new();
    for (index, review) in waiting.iter().enumerate() {
        let on = reviewer.on(review.chain)?;
        let id = U256::from(review.strategy_id);
        let reviews = on.deployments.reviews;
        let already = on
            .client
            .read(reviews, IReviewRegistry::reviewOfCall { strategyId: id })
            .await?;
        if already.review.reviewedAt != 0 {
            done.push(index);
            continue;
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
            continue;
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
        done.push(index);
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
    Ok(())
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
/// it must answer in.
async fn ask(
    model: &str,
    battery: &Battery,
    manifest: &Manifest,
    source: &str,
    sealed: bool,
) -> Result<Prose> {
    let description = format!(
        "Title: {}\n\n{}\n\nClaims:\n{}\n\nThe source is {}.",
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

/// The manifest at `path` inside a packed project.
fn manifest_in(archive: &[u8], path: &str) -> Result<Manifest> {
    let mut reader = tar::Archive::new(std::io::Cursor::new(archive));
    for entry in reader.entries()?.flatten() {
        let mut entry = entry;
        if entry.path()?.to_string_lossy() == path {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut entry, &mut text)?;
            return Manifest::from_toml(&text).map_err(|error| anyhow!(error));
        }
    }
    bail!("no {path} in the project")
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

    #[test]
    fn with_no_model_every_area_reads_couldnt_tell() {
        let (ratings, refused) = Battery::pinned().rate(&unread().answers);
        assert_eq!(ratings, [Rating::CouldntTell; 6]);
        assert!(!refused);
    }
}
