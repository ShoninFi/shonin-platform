//! Each chain's part of the record is read and written on its own: one
//! connected chain whose node has turned slow (not down), each request
//! taking seconds and a read dozens of them, holds back no other chain's
//! blocks, vaults and prices in the snapshot.
//!
//! Sequence: base and hyperliquid connect; hyperliquid's node then answers
//! each request after 8 s; base's answers at once. Within 20 s base's block
//! is in the snapshot, dated on its own clock.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;

/// A node that answers as chain `chain_id` at block 0x10, has no vaults and
/// no logs.
async fn a_node(chain_id: u64, slow: Arc<std::sync::atomic::AtomicBool>) -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(move |body: String| async move {
            if slow.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_secs(8)).await;
            }
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
            } else if body.contains("\"eth_getBlockByNumber\"") {
                let mut block =
                    alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                block.header.inner.number = 16;
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

async fn served(
    chain: shared::Chain,
    chain_id: u64,
    slow: Arc<std::sync::atomic::AtomicBool>,
) -> Served {
    let node = format!("http://{}", a_node(chain_id, slow).await);
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

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shonin-indexer-slow-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Base's block in the snapshot after 20 s of reading, and whether it is
/// dated, hyperliquid's node slow or not.
async fn base_block_after_20s(hyperliquid_slow: bool) -> Option<(u64, bool)> {
    let indexer = Arc::new(Indexer::new(
        vec![shared::Chain::Base, shared::Chain::Hyperliquid],
        scratch(),
        None,
    ));
    let fast = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let slow = Arc::new(std::sync::atomic::AtomicBool::new(false));
    indexer
        .add(served(shared::Chain::Base, 31337, fast).await)
        .await;
    indexer
        .add(served(shared::Chain::Hyperliquid, 31338, slow.clone()).await)
        .await;
    // Connected; now hyperliquid's node slows down.
    slow.store(hyperliquid_slow, std::sync::atomic::Ordering::SeqCst);
    let running = indexer.clone();
    let reading = tokio::spawn(async move { running.run().await });

    tokio::time::sleep(Duration::from_secs(20)).await;
    let block = indexer
        .snapshot
        .read()
        .await
        .chain(shared::Chain::Base)
        .map(|info| (info.block, info.as_of.is_some()));
    reading.abort();
    block
}

/// With both nodes answering at once, base's block is read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_both_nodes_quick_base_is_read() {
    assert_eq!(base_block_after_20s(false).await, Some((16, true)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_slow_chain_does_not_hold_back_another_chain_s_record() {
    let block = base_block_after_20s(true).await;
    assert_eq!(
        block,
        Some((16, true)),
        "after 20 s base's record still reads {block:?}: hyperliquid's slow node held it back"
    );
}
