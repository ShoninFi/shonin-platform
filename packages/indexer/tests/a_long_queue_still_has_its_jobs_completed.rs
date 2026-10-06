//! The build queue is completed with reads from each job's own chain, each
//! job's reads bounded on their own, and every job completed is kept from one
//! refresh to the next. The build service never forgets a job, so a chain's
//! list only grows: a queue longer than one refresh can complete is still
//! completed, a part at a time, and a job whose end is settled is not read
//! again once completed.
//!
//! Here base's node answers every read in 200 ms and gives every author the
//! handle `alice`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use alloy::sol_types::SolCall;
use axum::Router;
use axum::routing::{get, post};
use chain::bindings::profiles::{IAuthorProfiles, Profile};
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;
use serde_json::{Value, json};
use shared::model::registry::JobState;

const AUTHOR: &str = "0x00000000000000000000000000000000000000a1";

/// A node at block 0x10 with no vaults, answering every read after `delay`,
/// giving every author the handle `alice`, and counting how often it is asked
/// for a profile.
async fn a_slow_node(delay: Duration, profile_reads: Arc<AtomicUsize>) -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(move |axum::Json(request): axum::Json<Value>| async move {
            let id = request["id"].clone();
            let method = request["method"].as_str().unwrap_or_default().to_string();
            if method != "eth_chainId" {
                tokio::time::sleep(delay).await;
            }
            let result = match method.as_str() {
                "eth_chainId" => json!("0x7a69"),
                "eth_blockNumber" => json!("0x10"),
                "eth_getBlockByNumber" => {
                    let mut block =
                        alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                    block.header.inner.number = 16;
                    block.header.inner.timestamp = 1_700_000_000;
                    serde_json::to_value(block).unwrap()
                }
                "eth_call" => {
                    let tx = &request["params"][0];
                    let data = tx["input"]
                        .as_str()
                        .or_else(|| tx["data"].as_str())
                        .unwrap_or("0x");
                    let input = alloy::hex::decode(data).unwrap_or_default();
                    if input.starts_with(&IAuthorProfiles::profileOfCall::SELECTOR) {
                        profile_reads.fetch_add(1, Ordering::SeqCst);
                        json!(alloy::hex::encode_prefixed(
                            IAuthorProfiles::profileOfCall::abi_encode_returns(&Profile {
                                handle: "alice".to_string(),
                                blurb: String::new(),
                                linkLabels: Vec::new(),
                                linkUrls: Vec::new(),
                                joinedAt: 0,
                            })
                        ))
                    } else {
                        // `vaults()` and the like: an empty array.
                        json!(format!("0x{}{}", "0".repeat(62) + "20", "0".repeat(64)))
                    }
                }
                _ => json!([]),
            };
            axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

fn deployments() -> Deployments {
    let one = "0x0000000000000000000000000000000000000001";
    let mut fields = vec![
        r#""chain_key":"evm:31337""#.to_string(),
        r#""chain_id":31337"#.to_string(),
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
        "admin", "platform", "attestor", "builder", "reviewer", "executor", "keeper", "operator",
        "author", "viewer",
    ]
    .iter()
    .map(|role| format!(r#""{role}":"{one}""#))
    .collect();
    fields.push(format!(r#""roles":{{{}}}"#, roles.join(",")));
    Deployments::from_json(&format!("{{{}}}", fields.join(","))).unwrap()
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
    let node = format!(
        "http://{}",
        a_slow_node(Duration::from_millis(200), profile_reads.clone()).await
    );
    let config = ChainConfig {
        label: "stand-in".to_string(),
        rpc: node.clone(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        public_rpc: node,
        logs_rpc: None,
        log_range: None,
        confirmations: None,
    };
    let client = Client::connect(&config, Signing::ReadOnly).await.unwrap();
    let store = std::env::temp_dir().join(format!(
        "shonin-queue-completed-{}-{}",
        std::process::id(),
        jobs.len()
    ));
    let indexer = Arc::new(Indexer::new(
        vec![shared::Chain::Base],
        store,
        Some(a_builder_listing(jobs).await),
    ));
    indexer
        .add(Served {
            vault_chain: shared::Chain::Base,
            key: "evm:31337".parse::<ChainKey>().unwrap(),
            client,
            deployments: deployments(),
            native_usd: None,
            public_rpc: String::new(),
        })
        .await;
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

/// 300 jobs at 200 ms a read, 8 at a time, take more than one refresh to
/// read: two refreshes complete them all.
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
