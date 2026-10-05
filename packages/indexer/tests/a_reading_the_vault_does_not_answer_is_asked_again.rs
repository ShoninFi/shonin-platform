//! A reading the vault doesn't answer (a node failing a read now and then)
//! is asked for again at the next refresh, not passed over: passed over, the
//! record has no reading for that hour, for good.
//!
//! Here: a chain with 2 s blocks, the vault created at block 1, three hours
//! of chain time; the node fails the first two value reads asked of a past
//! block.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::post;
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;
use serde_json::{Value, json};

const DEPLOYED: u64 = 1;
const HEAD: u64 = 1 + 3 * 1_800;
const VAULT: &str = "00000000000000000000000000000000000000be";
const GENESIS_TIME: u64 = 1_700_000_000;

fn selector<C: alloy::sol_types::SolCall>() -> String {
    format!("0x{}", alloy::hex::encode(C::SELECTOR))
}

/// What the node made of the value reads asked of past blocks: the blocks it
/// failed, and those it answered.
#[derive(Default)]
struct Reads {
    failed: BTreeSet<u64>,
    answered: BTreeSet<u64>,
}

async fn a_node(reads: Arc<Mutex<Reads>>) -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(move |axum::Json(request): axum::Json<Value>| {
            let reads = reads.clone();
            async move {
                let id = request["id"].clone();
                let method = request["method"].as_str().unwrap_or_default().to_string();
                let answer = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
                let reply = match method.as_str() {
                    "eth_chainId" => answer(json!("0x7a69")),
                    "eth_blockNumber" => answer(json!(format!("{HEAD:#x}"))),
                    "eth_getBlockByNumber" => {
                        let number = request["params"][0]
                            .as_str()
                            .and_then(|tag| tag.strip_prefix("0x"))
                            .and_then(|hex| u64::from_str_radix(hex, 16).ok())
                            .unwrap_or(HEAD);
                        let mut block =
                            alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                        block.header.inner.number = number;
                        block.header.inner.timestamp = GENESIS_TIME + 2 * number;
                        answer(serde_json::to_value(block).unwrap())
                    }
                    "eth_call" => {
                        let at = request["params"][1]
                            .as_str()
                            .and_then(|tag| tag.strip_prefix("0x"))
                            .and_then(|hex| u64::from_str_radix(hex, 16).ok());
                        let data = request["params"][0]["input"]
                            .as_str()
                            .or_else(|| request["params"][0]["data"].as_str())
                            .unwrap_or("0x")
                            .to_ascii_lowercase();
                        let reading = data.starts_with(&selector::<
                            chain::bindings::vault::IVault::sharePriceCall,
                        >());
                        if let (true, Some(at)) = (reading, at) {
                            let mut reads = reads.lock().unwrap();
                            if reads.failed.len() < 2 && !reads.failed.contains(&at) {
                                reads.failed.insert(at);
                                return axum::Json(json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "error": {"code": -32000, "message": "header not found"}
                                }));
                            }
                            reads.answered.insert(at);
                        }
                        if data == selector::<chain::bindings::factory::IVaultFactory::vaultsCall>()
                        {
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
async fn a_reading_the_vault_does_not_answer_is_asked_again() {
    let reads = Arc::new(Mutex::new(Reads::default()));
    let node = format!("http://{}", a_node(reads.clone()).await);
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
    let store = std::env::temp_dir().join(format!(
        "shonin-indexer-unanswered-reading-{}",
        std::process::id()
    ));
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
    for _ in 0..4 {
        indexer.refresh().await.unwrap();
    }
    let reads = reads.lock().unwrap();
    assert!(!reads.failed.is_empty(), "no read was failed");
    let lost: Vec<u64> = reads.failed.difference(&reads.answered).copied().collect();
    assert!(
        lost.is_empty(),
        "the readings at blocks {lost:?} failed once and were never asked for again (answered: {:?})",
        reads.answered
    );
}
