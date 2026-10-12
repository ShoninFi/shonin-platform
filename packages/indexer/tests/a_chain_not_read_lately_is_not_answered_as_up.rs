//! The health route answers 503 while a vault chain it is to read is not
//! connected, or has not been read lately, and its first line, short enough
//! for a dashboard's row, says which: a chain that never connects is not
//! shown as up.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::{HEALTH_LINE, Indexer, stale_after};

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
        "shonin-indexer-health-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn first_line(said: &str) -> &str {
    said.lines().next().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_that_never_connects_is_not_answered_as_up() {
    let indexer = Indexer::new(
        vec![shared::Chain::Base, shared::Chain::Hyperliquid],
        scratch(),
        None,
    );
    indexer.add(served(shared::Chain::Base, 31337).await).await;
    tokio::time::timeout(Duration::from_secs(45), indexer.refresh())
        .await
        .expect("a refresh finishes")
        .unwrap();

    let (status, said) = indexer.health_answer("indexer");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{said}");
    assert_eq!(first_line(&said), "indexer: hyperliquid not connected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_read_lately_is_up_and_one_not_read_lately_is_not() {
    let indexer = Indexer::new(vec![shared::Chain::Base], scratch(), None);
    indexer.add(served(shared::Chain::Base, 31337).await).await;
    tokio::time::timeout(Duration::from_secs(45), indexer.refresh())
        .await
        .expect("a refresh finishes")
        .unwrap();

    let (status, said) = indexer.health_answer("indexer");
    assert_eq!(status, StatusCode::OK, "{said}");
    assert!(said.contains("base: read"), "{said}");

    // As it would read once the chain has gone unread past the limit.
    let later = Instant::now() + stale_after() + Duration::from_secs(60);
    let (up, said) = indexer.health_at("indexer", later);
    assert!(!up, "{said}");
    assert!(
        first_line(&said).starts_with("indexer: base not read for"),
        "{said}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_line_stays_short_however_many_chains_are_wrong() {
    let indexer = Indexer::new(
        vec![
            shared::Chain::Base,
            shared::Chain::Hyperliquid,
            shared::Chain::Robinhood,
            shared::Chain::Solana,
        ],
        scratch(),
        None,
    );
    let (up, said) = indexer.health_at("indexer", Instant::now());
    assert!(!up);
    assert_eq!(first_line(&said), "indexer: 4 chains not read");
    assert!(first_line(&said).chars().count() <= HEALTH_LINE);
    // The details follow, one chain a line.
    assert_eq!(said.lines().count(), 5, "{said}");
}
