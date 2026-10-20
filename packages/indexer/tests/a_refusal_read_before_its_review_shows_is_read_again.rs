//! The build service marks a job refused once the reviewer's refusal has a
//! receipt on the reviewer's node, and the indexer reads through its own,
//! which can be a block or two behind. A refusal completed before its review
//! shows on that node is not taken as settled: it is read again until its
//! review is read with it, so the public queue never shows a refusal without
//! its review for good.
//!
//! The build service lists a job as refused; the indexer's node does not
//! show the review yet, and one refresh completes the job without it. The
//! review then shows on the node, and the job carries it after the refreshes
//! that follow.

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alloy::sol_types::SolCall;
use axum::Router;
use axum::routing::get;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::profiles::{IAuthorProfiles, Profile};
use chain::bindings::reviews::IReviewRegistry;
use indexer::Indexer;
use serde_json::{Value, json};
use shared::model::registry::JobState;
use stand_in::*;

const AUTHOR: &str = "0x00000000000000000000000000000000000000a1";

/// A node with no vaults, giving every author the handle `alice`, and
/// showing the job's review once `reviewed` is set.
fn answering(reviewed: Arc<AtomicBool>) -> Answer {
    Arc::new(move |request: &Value| {
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
    })
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
async fn a_refusal_completed_before_its_review_is_readable_is_read_again() {
    let reviewed = Arc::new(AtomicBool::new(false));
    let served = served_on(
        Chain::at(HEAD),
        deployments_from(0),
        answering(reviewed.clone()),
    )
    .await;
    let store = std::env::temp_dir().join(format!("shonin-indexer-refused-{}", std::process::id()));
    let mut job = a_ready_job(0);
    job.state = JobState::Refused;
    let indexer = Arc::new(
        Indexer::new(
            vec![shared::Chain::Base],
            store,
            Some(a_builder_listing(vec![job]).await),
        )
        .with_rpc_policy(policy()),
    );
    indexer.add(served).await;

    indexer.refresh().await.unwrap();
    {
        let snapshot = indexer.snapshot.read().await;
        assert_eq!(
            snapshot.jobs[0].author, "alice",
            "the job was completed once"
        );
        assert!(snapshot.jobs[0].review.is_none());
    }
    // The refusal reaches the node the indexer reads.
    reviewed.store(true, Ordering::SeqCst);
    for _ in 0..3 {
        indexer.refresh().await.unwrap();
    }
    let snapshot = indexer.snapshot.read().await;
    assert!(
        snapshot.jobs[0].review.is_some(),
        "a refused job completed before its refusal was readable never shows its review"
    );
}
