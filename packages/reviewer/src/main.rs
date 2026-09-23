//! The reviewer.
//!
//! It asks a pinned model whether a strategy's code is reasonable and signs
//! the score it got back. The score is never a gate: nothing here can stop a
//! vault opening or a depositor depositing, and a low score is loud rather
//! than blocking. The prompt and the output schema are in this repository, so
//! an author can run the same review over their own code before submitting it.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use alloy::primitives::{B256, keccak256};
use alloy::rpc::types::Filter;
use alloy::sol_types::SolEvent;
use anyhow::{Context, Result, bail};
use chain::bindings::reviews::{IReviewRegistry, Review as SignedReview};
use chain::bindings::strategies::IStrategyRegistry;
use chain::{Client, Deployments};
use clap::Parser;
use serde::{Deserialize, Serialize};
use shared::manifest::hash_bytes;
use shared::model::review::Finding;
use tokio::io::AsyncWriteExt;

/// The prompt and the schema this image is pinned to. Changing either would
/// be an image change, behind the same timelock as any other.
const PROMPT: &str = include_str!("../review/prompt.md");
const SCHEMA: &str = include_str!("../review/schema.json");

#[derive(Parser)]
#[command(about = "Scores whether a strategy's code is reasonable")]
struct Args {
    #[arg(long, env = "SHONIN_RPC_URL")]
    rpc_url: String,
    #[arg(long)]
    deployments: PathBuf,
    #[arg(long)]
    store: PathBuf,
    /// The model this image is pinned to. Without it, every strategy simply
    /// shows "review pending", which is a state the site has to handle anyway.
    #[arg(long, env = "SHONIN_REVIEW_MODEL")]
    model: Option<String>,
    #[arg(long, default_value_t = 0)]
    port: u16,
}

/// What the model is asked to return, and what the store keeps.
#[derive(Clone, Serialize, Deserialize)]
struct Report {
    score: u8,
    summary: String,
    findings: Vec<Finding>,
    limits: Vec<String>,
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
    let chain = Client::connect(&args.rpc_url).await?;
    let deployments = chain.wait_for_deployments(&args.deployments).await?;
    let key = deployments.roles.reviewer;
    chain::attest::bless_service(&chain, &deployments, "reviewer", key).await?;
    chain::attest::keep_blessed(
        chain.clone(),
        deployments.clone(),
        "reviewer",
        key,
        chain::attest::Sender::Unlocked(key),
        alloy::primitives::Address::ZERO,
        B256::ZERO,
        || {},
    );

    let Some(model) = args.model.clone() else {
        tracing::info!("no model is pinned, so nothing is reviewed and every strategy says so");
        forever(args.port).await;
        return Ok(());
    };
    tracing::info!(%key, model, "the reviewer is up");

    if args.port != 0 {
        tokio::spawn(serve_status(SocketAddr::from(([127, 0, 0, 1], args.port))));
    }

    let mut refused: HashSet<(B256, u32, u32)> = HashSet::new();
    loop {
        if let Err(error) = sweep(&chain, &deployments, &args.store, &model, &mut refused).await {
            tracing::warn!(%error, "could not look for work");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// Anyone can buy a review at any time; every request that has not been served
/// is work.
async fn sweep(
    chain: &Client,
    deployments: &Deployments,
    store: &std::path::Path,
    model: &str,
    refused: &mut HashSet<(B256, u32, u32)>,
) -> Result<()> {
    let logs = chain
        .logs(
            &Filter::new()
                .address(deployments.reviews)
                .event_signature(IReviewRegistry::ReviewRequested::SIGNATURE_HASH)
                .from_block(deployments.deployed_block),
        )
        .await?;

    // Every request is one paid review. Counting the requests for a version
    // against the reviews the registry already holds is what decides whether
    // this one has been served, so a restart never re-reviews and a second
    // purchase is never dropped.
    let mut asked: HashMap<(B256, u32), u32> = HashMap::new();

    for log in logs {
        let Ok(request) = log.log_decode::<IReviewRegistry::ReviewRequested>() else {
            continue;
        };
        let (strategy_id, version) = (request.inner.strategyId, request.inner.version);
        let nth = {
            let counted = asked.entry((strategy_id, version)).or_default();
            *counted += 1;
            *counted
        };
        // A review this reviewer could not produce is not retried forever: it
        // is a line in the log and the next request's problem.
        if refused.contains(&(strategy_id, version, nth)) {
            continue;
        }

        let registered = chain
            .read(
                deployments.strategies,
                IStrategyRegistry::versionOfCall {
                    strategyId: strategy_id,
                    version,
                },
            )
            .await?;
        // A request can arrive before the build finishes; there is nothing to
        // read until it has.
        if registered.bundleHash == B256::ZERO {
            continue;
        }
        let already = chain
            .read(
                deployments.reviews,
                IReviewRegistry::reviewCountCall {
                    strategyId: strategy_id,
                    version,
                },
            )
            .await?;
        if already >= alloy::primitives::U256::from(nth) {
            continue;
        }

        match review_one(
            chain,
            deployments,
            store,
            model,
            strategy_id,
            version,
            &registered,
            request.inner.by,
        )
        .await
        {
            Ok(score) => tracing::info!(version, score, nth, "reviewed"),
            Err(error) => {
                tracing::warn!(%error, "could not review this one");
                refused.insert((strategy_id, version, nth));
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn review_one(
    chain: &Client,
    deployments: &Deployments,
    store: &std::path::Path,
    model: &str,
    strategy_id: B256,
    version: u32,
    registered: &chain::bindings::strategies::StrategyVersion,
    bought_by: alloy::primitives::Address,
) -> Result<u8> {
    let source_hash = format!("sha256:{}", alloy::hex::encode(registered.sourceHash));
    let source = read_source(store, &source_hash)
        .context("the source is not in the store; a sealed strategy is still reviewed, and this one has no bytes here")?;

    let report = ask(model, &source).await?;
    let body = serde_json::to_vec_pretty(&report)?;
    let report_hash = hash_bytes(&body);
    write(store, &report_hash, "report.json", &body)?;

    let signed = SignedReview {
        bundleHash: registered.bundleHash,
        sourceHash: registered.sourceHash,
        reviewerImage: chain::attest::image_digest("reviewer"),
        modelIdHash: keccak256(model.as_bytes()),
        // The prompt and the output schema together: a review is what the
        // model was asked and the shape it had to answer in, so changing
        // either changes the review this one can be compared with.
        promptHash: keccak256([PROMPT.as_bytes(), SCHEMA.as_bytes()].concat()),
        reportHash: hash_to_bytes(&report_hash)?,
        score: report.score,
        findingCount: report.findings.len() as u16,
        reviewedAt: chain.block_timestamp().await? as u64,
        boughtBy: bought_by,
    };
    chain
        .send(
            deployments.roles.reviewer,
            deployments.reviews,
            IReviewRegistry::submitReviewCall {
                strategyId: strategy_id,
                version,
                review: signed,
            },
        )
        .await
        .context("the registry would not take the review")?;
    Ok(report.score)
}

/// Run the pinned model over the source, with the schema it must answer in.
async fn ask(model: &str, source: &str) -> Result<Report> {
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
            .write_all(format!("{PROMPT}\n\n{source}\n").as_bytes())
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
    let report: Report =
        serde_json::from_value(structured).context("the report did not match the schema")?;
    Ok(sanitise(report))
}

/// The model answers inside a schema, and the score is still clamped here: a
/// reviewer's own bounds should not be something a prompt can argue with.
fn sanitise(mut report: Report) -> Report {
    report.score = report.score.min(100);
    report.findings.truncate(50);
    if report.limits.is_empty() {
        report.limits =
            vec!["A review is one reading of the code, not a proof about it.".to_string()];
    }
    report
}

/// Every file of the submitted project, in full, with line numbers so a
/// finding can point at one.
fn read_source(store: &std::path::Path, source_hash: &str) -> Result<String> {
    // Sealed or not, the reviewer reads the code: sealing decides who else may
    // read it, never whether it was read.
    let held = store.join(source_hash.replace(':', "-"));
    let published = held.join("source.tar");
    let sealed = held.join("source.sealed.tar");
    let archive = if published.exists() {
        std::fs::read(published)?
    } else {
        std::fs::read(sealed)?
    };
    let mut reader = tar::Archive::new(std::io::Cursor::new(archive));
    let mut out = String::new();
    for entry in reader.entries()?.flatten() {
        let path = entry.path()?.display().to_string();
        if !(path.ends_with(".rs") || path.ends_with(".toml")) {
            continue;
        }
        let mut entry = entry;
        let mut body = String::new();
        use std::io::Read;
        if entry.read_to_string(&mut body).is_err() {
            continue;
        }
        out.push_str(&format!("\n----- {path} -----\n"));
        for (number, line) in body.lines().enumerate() {
            out.push_str(&format!("{:>4} {line}\n", number + 1));
        }
    }
    if out.is_empty() {
        bail!("there is no source to read");
    }
    Ok(out)
}

fn write(store: &std::path::Path, hash: &str, name: &str, bytes: &[u8]) -> Result<()> {
    let directory = store.join(hash.replace(':', "-"));
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join(name), bytes)?;
    Ok(())
}

fn hash_to_bytes(hash: &str) -> Result<B256> {
    let bytes = shared::manifest::hash_to_bytes(hash).context("a hash")?;
    Ok(B256::from(bytes))
}

async fn serve_status(address: SocketAddr) {
    let app = axum::Router::new().route("/health", axum::routing::get(|| async { "reviewer" }));
    if let Ok(listener) = tokio::net::TcpListener::bind(address).await {
        let _ = axum::serve(listener, app).await;
    }
}

async fn forever(port: u16) {
    if port != 0 {
        serve_status(SocketAddr::from(([127, 0, 0, 1], port))).await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// Keep the categories and severities the schema names in step with the model
/// the rest of the platform reads.
#[cfg(test)]
mod tests {
    use super::*;
    use shared::model::review::{FindingCategory, Severity};

    #[test]
    fn every_category_the_schema_names_is_one_the_platform_knows() {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).expect("the schema parses");
        let names = schema["properties"]["findings"]["items"]["properties"]["category"]["enum"]
            .as_array()
            .expect("the categories are listed");
        assert_eq!(names.len(), FindingCategory::all().len());
        for name in names {
            let text = format!("\"{}\"", name.as_str().expect("a name"));
            serde_json::from_str::<FindingCategory>(&text).expect("the platform knows it");
        }
    }

    #[test]
    fn every_severity_the_schema_names_is_one_the_platform_knows() {
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).expect("the schema parses");
        let names = schema["properties"]["findings"]["items"]["properties"]["severity"]["enum"]
            .as_array()
            .expect("the severities are listed");
        for name in names {
            let text = format!("\"{}\"", name.as_str().expect("a name"));
            serde_json::from_str::<Severity>(&text).expect("the platform knows it");
        }
    }

    #[test]
    fn a_score_past_a_hundred_is_brought_back() {
        let report = sanitise(Report {
            score: 240,
            summary: String::new(),
            findings: Vec::new(),
            limits: Vec::new(),
        });
        assert_eq!(report.score, 100);
        assert!(!report.limits.is_empty());
    }
}
