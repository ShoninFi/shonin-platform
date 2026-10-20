//! The build queue is completed with reads from each job's own chain, each
//! job's reads bounded on their own, and every job completed is kept from one
//! refresh to the next. The build service never forgets a job, so a chain's
//! list only grows: a queue longer than one refresh can complete is still
//! completed, a part at a time, and a job whose end is settled is not read
//! again once completed.
//!
//! Each chain's jobs are read in one observation of it, so a long queue on
//! one chain costs one read of it.
//!
//! Here base's node answers every read in 200 ms and gives every author the
//! handle `alice`.

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use alloy::sol_types::SolCall;
use axum::Router;
use axum::routing::get;
use chain::Served;
use chain::bindings::profiles::{IAuthorProfiles, Profile};
use indexer::Indexer;
use serde_json::{Value, json};
use shared::model::registry::JobState;
use stand_in::*;

const AUTHOR: &str = "0x00000000000000000000000000000000000000a1";

/// Base, on a node at block 0x10 with no vaults, answering every read after
/// `delay`, giving every author the handle `alice`, and counting how often
/// it is asked for a profile.
async fn a_slow_chain(delay: Duration, profile_reads: Arc<AtomicUsize>) -> Served {
    let answer: Answer = Arc::new(move |request: &Value| {
        if request["method"] != "eth_call" {
            return None;
        }
        let data = input(request);
        if data.starts_with(&selector::<IAuthorProfiles::profileOfCall>()) {
            profile_reads.fetch_add(1, Ordering::SeqCst);
            return Some(Ok(json!(alloy::hex::encode_prefixed(
                IAuthorProfiles::profileOfCall::abi_encode_returns(&Profile {
                    handle: "alice".to_string(),
                    blurb: String::new(),
                    linkLabels: Vec::new(),
                    linkUrls: Vec::new(),
                    joinedAt: 0,
                })
            ))));
        }
        // `vaults()` and the like: an empty array.
        Some(Ok(no_vaults()))
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

fn a_job(id: usize, state: JobState) -> shared::model::registry::Job {
    shared::model::registry::Job {
        id: (8_800 + id).to_string(),
        chain: shared::Chain::Base,
        strategy_id: None,
        title: format!("job {id}"),
        author: AUTHOR.to_string(),
        state,
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

/// The authors the snapshot's queue shows after `refreshes` refreshes, with
/// the build service listing `jobs` on base, and how often base's node was
/// asked for a profile.
async fn authors_shown(
    jobs: Vec<shared::model::registry::Job>,
    refreshes: usize,
) -> (Vec<String>, usize) {
    let profile_reads = Arc::new(AtomicUsize::new(0));
    let served = a_slow_chain(Duration::from_millis(200), profile_reads.clone()).await;
    let store = std::env::temp_dir().join(format!(
        "shonin-queue-completed-{}-{}",
        std::process::id(),
        jobs.len()
    ));
    let indexer = Arc::new(
        Indexer::new(
            vec![shared::Chain::Base],
            store,
            Some(a_builder_listing(jobs).await),
        )
        .with_rpc_policy(policy()),
    );
    indexer.add(served).await;
    for _ in 0..refreshes {
        indexer.refresh().await.unwrap();
    }
    let snapshot = indexer.snapshot.read().await;
    (
        snapshot.jobs.iter().map(|job| job.author.clone()).collect(),
        profile_reads.load(Ordering::SeqCst),
    )
}

fn unpaid(count: usize) -> Vec<shared::model::registry::Job> {
    (0..count)
        .map(|id| a_job(id, JobState::AwaitingPayment))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_short_queue_is_completed() {
    assert_eq!(authors_shown(unpaid(1), 2).await.0, ["alice"]);
}

/// 300 jobs on a node answering each read in 200 ms: two refreshes complete
/// them all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_queue_still_has_its_jobs_completed() {
    let (shown, _) = authors_shown(unpaid(300), 2).await;
    let completed = shown.iter().filter(|author| *author == "alice").count();
    assert_eq!(
        completed, 300,
        "{completed} of 300 jobs on one chain were completed after two refreshes: what one \
         refresh completes is not kept for the next"
    );
}

/// A refused job's chain has nothing more to add to it: once completed, it
/// is not read again however often the queue is refreshed, and still shows
/// what was read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_settled_job_once_completed_is_not_read_again() {
    let (shown, profile_reads) = authors_shown(vec![a_job(0, JobState::Refused)], 4).await;
    assert_eq!(shown, ["alice"]);
    assert_eq!(
        profile_reads, 1,
        "a refused job's author was read {profile_reads} times over four refreshes"
    );
}

/// A job still on its way is read again at each refresh: its chain may yet
/// add to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_still_on_its_way_is_read_again() {
    let (_, profile_reads) = authors_shown(unpaid(1), 3).await;
    assert_eq!(profile_reads, 3);
}
