//! A build service that takes the connection and never answers (hung, or a
//! proxy holding it) holds a refresh for no longer than a request's timeout:
//! the record of every chain keeps moving, and so does the API's snapshot,
//! queue and every portfolio read from it. One that stops answering leaves
//! the queue it last gave, marked stale and dated as it was.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;

/// A node that answers as chain `chain_id` at block 0x10, has no vaults and
/// no logs.
async fn a_node(chain_id: u64) -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(move |body: String| async move {
            let id = body
                .split("\"id\":")
                .nth(1)
                .map(|rest| {
                    rest.chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect::<String>()
                })
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| "0".to_string());
            let result = if body.contains("\"eth_chainId\"") {
                format!("\"0x{chain_id:x}\"")
            } else if body.contains("\"eth_blockNumber\"") {
                "\"0x10\"".to_string()
            } else if body.contains("\"eth_getBlockByNumber\"")
                || body.contains("\"eth_getBlockByHash\"")
            {
                let mut block =
                    alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                let request: serde_json::Value = serde_json::from_str(&body).unwrap();
                let tag = request["params"][0].as_str().unwrap();
                let number = if request["method"] == "eth_getBlockByHash" {
                    tag.parse::<alloy::primitives::B256>().unwrap()[0] as u64
                } else {
                    tag.strip_prefix("0x")
                        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
                        .unwrap_or(16)
                };
                block.header.inner.number = number;
                block.header.hash = alloy::primitives::B256::repeat_byte(number as u8);
                block.header.inner.parent_hash = if number == 0 {
                    alloy::primitives::B256::ZERO
                } else {
                    alloy::primitives::B256::repeat_byte((number - 1) as u8)
                };
                block.header.inner.timestamp = 1_700_000_000;
                serde_json::to_string(&block).unwrap()
            } else if body.contains("\"eth_call\"") {
                // `vaults()`: an empty address[].
                format!("\"0x{}{}\"", "0".repeat(62) + "20", "0".repeat(64))
            } else {
                "[]".to_string()
            };
            (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

/// A build service that takes every connection and never says a word.
async fn a_builder_that_never_answers() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            if let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        }
    });
    format!("http://{address}")
}

fn deployments(chain_id: u64) -> Deployments {
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
        "admin", "platform", "attestor", "builder", "reviewer", "executor", "keeper", "operator",
        "author", "viewer",
    ]
    .iter()
    .map(|role| format!(r#""{role}":"{one}""#))
    .collect();
    fields.push(format!(r#""roles":{{{}}}"#, roles.join(",")));
    Deployments::from_json(&format!("{{{}}}", fields.join(","))).unwrap()
}

async fn served(chain: shared::Chain, chain_id: u64) -> Served {
    let node = format!("http://{}", a_node(chain_id).await);
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
    Served {
        vault_chain: chain,
        key: format!("evm:{chain_id}").parse::<ChainKey>().unwrap(),
        client,
        deployments: deployments(chain_id),
        native_usd: None,
        public_rpc: String::new(),
    }
}

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shonin-indexer-builder-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A build service that isn't there at all (refused at once)
/// holds nothing up, and base's block is read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_service_that_is_down_holds_nothing_up() {
    let indexer = Indexer::new(
        vec![shared::Chain::Base],
        scratch(),
        Some("http://127.0.0.1:9".to_string()),
    );
    indexer.add(served(shared::Chain::Base, 31337).await).await;
    tokio::time::timeout(Duration::from_secs(45), indexer.refresh())
        .await
        .expect("a refresh finishes")
        .unwrap();
    let block = indexer
        .snapshot
        .read()
        .await
        .chain(shared::Chain::Base)
        .map(|info| info.block);
    assert_eq!(block, Some(16));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_service_that_never_answers_does_not_freeze_every_chain_s_record() {
    let builder = a_builder_that_never_answers().await;
    let indexer = Arc::new(Indexer::new(
        vec![shared::Chain::Base],
        scratch(),
        Some(builder),
    ));
    indexer.add(served(shared::Chain::Base, 31337).await).await;

    // Well past any request's own timeout.
    let refreshed = tokio::time::timeout(Duration::from_secs(45), indexer.refresh()).await;
    let block = indexer
        .snapshot
        .read()
        .await
        .chain(shared::Chain::Base)
        .map(|info| info.block);
    assert!(
        refreshed.is_ok(),
        "a refresh was still waiting on the build service after 45 s; base's block in the \
         snapshot is {block:?} although its node answers at once"
    );
}

/// A build service that lists one job on base the first time it is asked,
/// and fails every time after.
async fn a_builder_that_answers_once() -> String {
    let job = shared::model::registry::Job {
        id: "8801".to_string(),
        chain: shared::Chain::Base,
        strategy_id: None,
        title: "job 8801".to_string(),
        author: "someone".to_string(),
        state: shared::model::registry::JobState::Ready,
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
    };
    let body = serde_json::to_string(&vec![job]).unwrap();
    let asked = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let app = Router::new().route(
        "/jobs",
        get(move || {
            let body = body.clone();
            let asked = asked.clone();
            async move {
                if asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    (axum::http::StatusCode::OK, body)
                } else {
                    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, String::new())
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_service_that_stops_answering_leaves_its_last_queue_marked_stale() {
    let indexer = Indexer::new(
        vec![shared::Chain::Base],
        scratch(),
        Some(a_builder_that_answers_once().await),
    );
    indexer.add(served(shared::Chain::Base, 31337).await).await;
    indexer.refresh().await.unwrap();
    let (jobs, as_of, stale) = {
        let snapshot = indexer.snapshot.read().await;
        (
            snapshot.jobs.len(),
            snapshot.jobs_as_of,
            snapshot.jobs_stale,
        )
    };
    assert_eq!(jobs, 1);
    assert!(as_of.is_some() && !stale);

    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await;
    assert_eq!(
        snapshot
            .jobs
            .iter()
            .map(|job| job.id.as_str())
            .collect::<Vec<_>>(),
        ["8801"],
        "the queue went blank when the build service failed"
    );
    assert!(snapshot.jobs_stale, "the queue kept is not marked stale");
    assert_eq!(
        snapshot.jobs_as_of, as_of,
        "the queue kept is not dated as it was"
    );
}
