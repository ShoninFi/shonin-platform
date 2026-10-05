//! The build queue is completed with reads from each job's own chain (the
//! author's handle, the review, whether it has a vault). A chain whose node
//! takes the connection and never answers holds only its own jobs, and those
//! for a bounded time: every other chain's jobs reach the queue, and a job on
//! the hanging chain is still answered, with what was last known of it.
//!
//! Base's node answers at once; robinhood's answers only `eth_chainId` (so it
//! connects) and then never again. The build service lists one job on each.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;

/// A node that answers as chain `chain_id` at block 0x10, has no vaults and
/// no logs. With `hangs`, it answers only `eth_chainId` and holds every
/// other request open for good.
async fn a_node(chain_id: u64, hangs: bool) -> SocketAddr {
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
            if hangs && !body.contains("\"eth_chainId\"") {
                tokio::time::sleep(Duration::from_secs(3_600)).await;
            }
            let result = if body.contains("\"eth_chainId\"") {
                format!("\"0x{chain_id:x}\"")
            } else if body.contains("\"eth_blockNumber\"") {
                "\"0x10\"".to_string()
            } else if body.contains("\"eth_getBlockByNumber\"") {
                let mut block =
                    alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                block.header.inner.number = 16;
                block.header.inner.timestamp = 1_700_000_000;
                serde_json::to_string(&block).unwrap()
            } else if body.contains("\"eth_call\"") {
                // `vaults()`: an empty address[]. Anything else read with it
                // fails to decode, which `read::job` passes over.
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

async fn served(chain: shared::Chain, chain_id: u64, hangs: bool) -> Served {
    let node = format!("http://{}", a_node(chain_id, hangs).await);
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
    Served {
        vault_chain: chain,
        key: format!("evm:{chain_id}").parse::<ChainKey>().unwrap(),
        client,
        deployments: deployments(chain_id),
        native_usd: None,
        public_rpc: String::new(),
    }
}

fn a_job(id: &str, chain: shared::Chain, strategy_id: Option<u64>) -> shared::model::registry::Job {
    shared::model::registry::Job {
        id: id.to_string(),
        chain,
        strategy_id,
        title: format!("job {id}"),
        author: "0x00000000000000000000000000000000000000a1".to_string(),
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
    }
}

/// A build service listing one job on robinhood and one on base.
async fn a_builder() -> String {
    let jobs = vec![
        a_job("8800", shared::Chain::Robinhood, Some(1)),
        a_job("8801", shared::Chain::Base, Some(2)),
    ];
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

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shonin-indexer-slow-chain-jobs-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An indexer reading base, and robinhood (hanging or not), with the build
/// service's queue.
async fn an_indexer(robinhood_hangs: bool) -> Arc<Indexer> {
    let indexer = Arc::new(Indexer::new(
        vec![shared::Chain::Base, shared::Chain::Robinhood],
        scratch(),
        Some(a_builder().await),
    ));
    indexer
        .add(served(shared::Chain::Base, 31337, false).await)
        .await;
    indexer
        .add(served(shared::Chain::Robinhood, 31338, robinhood_hangs).await)
        .await;
    indexer
}

/// Whether base's job reaches the snapshot within 20 s, with robinhood's
/// node hanging or not.
async fn base_job_seen(robinhood_hangs: bool) -> (bool, Option<u64>) {
    let indexer = an_indexer(robinhood_hangs).await;
    let running = indexer.clone();
    let reading = tokio::spawn(async move { running.run().await });

    let started = std::time::Instant::now();
    let mut seen = false;
    while started.elapsed() < Duration::from_secs(20) {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if indexer
            .snapshot
            .read()
            .await
            .jobs
            .iter()
            .any(|job| job.id == "8801")
        {
            seen = true;
            break;
        }
    }
    let base_block = indexer
        .snapshot
        .read()
        .await
        .chain(shared::Chain::Base)
        .map(|info| info.block);
    reading.abort();
    (seen, base_block)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_every_chain_answering_the_queue_is_read() {
    assert!(base_job_seen(false).await.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_slow_chain_holds_up_no_other_chain_s_jobs() {
    let (seen, base_block) = base_job_seen(true).await;
    assert!(
        seen,
        "20 s on, base's job is still not in the queue (base's record reads block \
         {base_block:?}): the queue waits on robinhood's node, which never answers"
    );
}

/// One job asked for on its own (as the API's `/api/jobs/{id}` does) on a
/// chain that never answers is still answered, in bounded time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_on_a_chain_that_never_answers_is_still_answered() {
    let indexer = an_indexer(true).await;
    let job = a_job("8800", shared::Chain::Robinhood, Some(1));
    let answered = tokio::time::timeout(Duration::from_secs(20), indexer.complete(job)).await;
    let job = answered.expect("20 s on, the job on robinhood is still waiting on its node");
    assert_eq!(job.id, "8800");
    assert_eq!(job.state, shared::model::registry::JobState::Ready);
}
