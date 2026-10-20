//! On a chain slow enough that a refresh cuts some of its job reads off,
//! every unsettled job is still read again in turn, so what the chain later
//! adds to any of them (here, its review being posted) is shown.
//!
//! Jobs are read in the order they were last completed, never-completed
//! first. A read cut off at the refresh's limit does not count as one, so the
//! jobs it was cut off from keep their place at the front.
//!
//! Here base's node reads every view on its own (it runs no batched read),
//! each in 120 ms, so eight jobs take about two seconds and a refresh (5 s)
//! completes about sixteen of the twenty-four `Ready` jobs.
//! 1. two refreshes complete every job once.
//! 2. every job's review is posted on chain.
//! 3. after four more refreshes, every job shows its review.

mod stand_in;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use alloy::sol_types::SolCall;
use axum::Router;
use axum::routing::get;
use chain::Served;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::profiles::{IAuthorProfiles, Profile};
use chain::bindings::reviews::IReviewRegistry;
use indexer::Indexer;
use serde_json::{Value, json};
use shared::model::registry::JobState;
use stand_in::*;

const AUTHOR: &str = "0x00000000000000000000000000000000000000a1";

/// How many jobs the build service lists.
const JOBS: u64 = 24;

/// Base, on a node with no vaults answering every read after `delay`,
/// giving every author the handle `alice`, and every job's review once
/// `reviewed` is set.
async fn a_slow_chain(delay: Duration, reviewed: Arc<AtomicBool>) -> Served {
    let answer: Answer = Arc::new(move |request: &Value| {
        if request["method"] != "eth_call" {
            return None;
        }
        let input = alloy::hex::decode(input(request)).unwrap_or_default();
        Some(Ok(
            if input.starts_with(&IAuthorProfiles::profileOfCall::SELECTOR) {
                json!(alloy::hex::encode_prefixed(
                    IAuthorProfiles::profileOfCall::abi_encode_returns(&Profile {
                        handle: "alice".to_string(),
                        blurb: String::new(),
                        linkLabels: Vec::new(),
                        linkUrls: Vec::new(),
                        joinedAt: 0,
                    })
                ))
            } else if input.starts_with(&IReviewRegistry::reviewOfCall::SELECTOR) {
                // A static tuple: 14 words of review (6 hashes, 6 ratings, findingCount, reviewedAt), then `refused`.
                let mut words = vec![0u8; 15 * 32];
                if reviewed.load(Ordering::SeqCst) {
                    // reviewedAt, the review's last word.
                    words[13 * 32 + 31] = 1;
                }
                json!(alloy::hex::encode_prefixed(words))
            } else if input.starts_with(&IVaultFactory::vaultOfCall::SELECTOR) {
                json!(format!("0x{}", "0".repeat(64)))
            } else {
                no_vaults()
            },
        ))
    });
    let node = a_slow_node_on(Chain::at(HEAD), delay, answer).await;
    Served {
        vault_chain: shared::Chain::Base,
        key: "evm:31337".parse().unwrap(),
        client: client(&node).await,
        deployments: deployments_from(0),
        native_usd: None,
        public_rpc: String::new(),
    }
}

fn a_ready_job(id: u64) -> shared::model::registry::Job {
    shared::model::registry::Job {
        id: (9_100 + id).to_string(),
        chain: shared::Chain::Base,
        strategy_id: Some(id + 1),
        title: format!("job {id}"),
        author: AUTHOR.to_string(),
        state: JobState::Ready,
        submitted: time::PrimitiveDateTime::new(
            time::Date::from_ordinal_date(2026, 276).unwrap(),
            time::Time::MIDNIGHT,
        ),
        duration_seconds: 0,
        source_hash: "sha256:00".to_string(),
        sealed: false,
        bundle_hash: None,
        review: None,
        strategy_slug: None,
    }
}

async fn a_builder_listing(jobs: Vec<shared::model::registry::Job>) -> String {
    let body = serde_json::to_string(&jobs).unwrap();
    let app = Router::new().route(
        "/jobs",
        get(move || {
            let body = body.clone();
            async move {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_unsettled_job_is_read_again_on_a_slow_chain() {
    let reviewed = Arc::new(AtomicBool::new(false));
    let served = a_slow_chain(Duration::from_millis(120), reviewed.clone()).await;
    let store =
        std::env::temp_dir().join(format!("shonin-unsettled-in-turn-{}", std::process::id()));
    let jobs: Vec<_> = (0..JOBS).map(a_ready_job).collect();
    let indexer = Arc::new(
        Indexer::new(
            vec![shared::Chain::Base],
            store,
            Some(a_builder_listing(jobs).await),
        )
        .with_rpc_policy(policy()),
    );
    indexer.add(served).await;

    for _ in 0..2 {
        indexer.refresh().await.unwrap();
    }
    {
        let snapshot = indexer.snapshot.read().await;
        let completed = snapshot
            .jobs
            .iter()
            .filter(|job| job.author == "alice")
            .count();
        assert_eq!(
            completed, JOBS as usize,
            "every job is completed once after two refreshes"
        );
    }

    // The reviews are posted.
    reviewed.store(true, Ordering::SeqCst);
    for _ in 0..4 {
        indexer.refresh().await.unwrap();
    }
    let snapshot = indexer.snapshot.read().await;
    let shown: HashMap<String, bool> = snapshot
        .jobs
        .iter()
        .map(|job| (job.id.clone(), job.review.is_some()))
        .collect();
    let missing: Vec<&String> = shown
        .iter()
        .filter(|(_, reviewed)| !**reviewed)
        .map(|(id, _)| id)
        .collect();
    assert!(
        missing.is_empty(),
        "after four refreshes, {} of {JOBS} unsettled jobs still show no review: {missing:?}",
        missing.len()
    );
}
