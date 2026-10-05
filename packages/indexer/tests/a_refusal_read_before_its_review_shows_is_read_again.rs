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

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use alloy::sol_types::SolCall;
use axum::Router;
use axum::routing::{get, post};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::profiles::{IAuthorProfiles, Profile};
use chain::bindings::reviews::IReviewRegistry;
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;
use serde_json::{Value, json};
use shared::model::registry::JobState;

const AUTHOR: &str = "0x00000000000000000000000000000000000000a1";

async fn a_slow_node(delay: Duration, reviewed: Arc<AtomicBool>) -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(move |axum::Json(request): axum::Json<Value>| {
            let reviewed = reviewed.clone();
            async move {
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
                            json!(format!("0x{}{}", "0".repeat(62) + "20", "0".repeat(64)))
                        }
                    }
                    _ => json!([]),
                };
                axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
            }
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
    let node = format!(
        "http://{}",
        a_slow_node(Duration::from_millis(0), reviewed.clone()).await
    );
    let config = ChainConfig {
        label: "stand-in".to_string(),
        rpc: node.clone(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        public_rpc: node,
        confirmations: None,
    };
    let client = Client::connect(&config, Signing::ReadOnly).await.unwrap();
    let store = std::env::temp_dir().join(format!("shonin-indexer-refused-{}", std::process::id()));
    let mut job = a_ready_job(0);
    job.state = JobState::Refused;
    let indexer = Arc::new(Indexer::new(
        vec![shared::Chain::Base],
        store,
        Some(a_builder_listing(vec![job]).await),
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
