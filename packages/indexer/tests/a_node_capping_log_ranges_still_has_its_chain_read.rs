//! Most providers cap how many blocks one `eth_getLogs` may span (10,000
//! is common; some cap at 1,000 or 2,000). Every log the indexer reads (the
//! feed's registrations and image proposals, the vaults' activations, each
//! vault's events, fills and blessings) is read a page at a time, a page the
//! node refuses halved until it is taken, so such a node still has its
//! chain read: the record is written, the vault is in the snapshot, and the
//! chain answers as up.
//!
//! Here: a node refusing a log request spanning more than 10,000 blocks,
//! the platform deployed 20,000 blocks ago, one vault.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;
use serde_json::{Value, json};

const DEPLOYED: u64 = 1_000_000;
const HEAD: u64 = DEPLOYED + 20_000;
const RANGE_CAP: u64 = 10_000;
const VAULT: &str = "00000000000000000000000000000000000000be";

fn block_of(tag: &Value, default: u64) -> u64 {
    tag.as_str()
        .and_then(|tag| tag.strip_prefix("0x"))
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
        .unwrap_or(default)
}

async fn a_node() -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(move |axum::Json(request): axum::Json<Value>| async move {
            let id = request["id"].clone();
            let method = request["method"].as_str().unwrap_or_default().to_string();
            let answer = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
            let reply = match method.as_str() {
                "eth_chainId" => answer(json!("0x7a69")),
                "eth_blockNumber" => answer(json!(format!("{HEAD:#x}"))),
                "eth_getBlockByNumber" => {
                    let mut block =
                        alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                    block.header.inner.number = HEAD;
                    block.header.inner.timestamp = 1_700_000_000;
                    answer(serde_json::to_value(block).unwrap())
                }
                "eth_getLogs" => {
                    let filter = &request["params"][0];
                    let from = block_of(&filter["fromBlock"], 0);
                    let to = block_of(&filter["toBlock"], HEAD);
                    if to.saturating_sub(from) > RANGE_CAP {
                        json!({"jsonrpc": "2.0", "id": id, "error": {
                            "code": -32005,
                            "message": "query exceeds max block range 10000"
                        }})
                    } else {
                        answer(json!([]))
                    }
                }
                "eth_call" => {
                    let data = request["params"][0]["input"]
                        .as_str()
                        .or_else(|| request["params"][0]["data"].as_str())
                        .unwrap_or("0x")
                        .to_ascii_lowercase();
                    let vaults = format!(
                        "0x{}",
                        alloy::hex::encode(
                            <chain::bindings::factory::IVaultFactory::vaultsCall
                                as alloy::sol_types::SolCall>::SELECTOR
                        )
                    );
                    if data == vaults {
                        answer(json!(format!(
                            "0x{}{}{}",
                            "0".repeat(62) + "20",
                            "0".repeat(63) + "1",
                            "0".repeat(24) + VAULT
                        )))
                    } else {
                        answer(json!(format!("0x{}", "0".repeat(64 * 128))))
                    }
                }
                _ => answer(json!([])),
            };
            axum::Json(reply)
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
        format!(r#""deployed_block":{DEPLOYED}"#),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_capping_log_ranges_still_has_its_chain_read() {
    let node = format!("http://{}", a_node().await);
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
    let store = std::env::temp_dir().join(format!("shonin-indexer-log-cap-{}", std::process::id()));
    let indexer = Arc::new(Indexer::new(vec![shared::Chain::Base], store, None));
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

    tokio::time::timeout(Duration::from_secs(30), indexer.refresh())
        .await
        .expect("a refresh ends")
        .unwrap();
    let snapshot = indexer.snapshot.read().await.clone();
    let block = snapshot
        .chain(shared::Chain::Base)
        .map(|info| info.block)
        .unwrap_or(0);
    let vaults = snapshot.strategies.len();
    let (up, said) = indexer.health_at("indexer", std::time::Instant::now());
    assert!(
        block == HEAD && vaults == 1 && up,
        "chain read to block {block} (head {HEAD}); vaults in the record: {vaults}; \
         health: {said:?}"
    );
}
