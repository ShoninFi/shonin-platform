//! Each of a vault's readings is taken at the first block its hour falls
//! in, not at whichever block a guess from the chain's average pace lands on
//! once that block's time is due: on a chain whose pace changes (one quiet
//! for days, then busy) such a guess overshoots, and days of the record
//! would have no reading.
//!
//! Here: the vault is created at block 1; blocks 1..=1,000 come one every
//! 864 s (ten quiet days), then one a second. The average pace from block 1
//! to the head puts the hour after block 1 near block 378, nearly four days
//! on.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::routing::post;
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;
use serde_json::{Value, json};

const DEPLOYED: u64 = 1;
const QUIET: u64 = 1_000;
const QUIET_GAP: u64 = 864;

fn time_of(block: u64) -> u64 {
    if block <= QUIET {
        GENESIS_TIME + QUIET_GAP * block
    } else {
        GENESIS_TIME + QUIET_GAP * QUIET + (block - QUIET)
    }
}
const VAULT: &str = "00000000000000000000000000000000000000be";
const GENESIS_TIME: u64 = 1_700_000_000;

fn selector<C: alloy::sol_types::SolCall>() -> String {
    format!("0x{}", alloy::hex::encode(C::SELECTOR))
}

/// A node whose head is `head`, its blocks paced as [`time_of`] says. Keeps
/// the block of each `sharePrice` read asked of a past block: one per sample.
async fn a_node(head: Arc<AtomicU64>, samples: Arc<std::sync::Mutex<Vec<u64>>>) -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(move |axum::Json(request): axum::Json<Value>| {
            let head = head.clone();
            let samples = samples.clone();
            async move {
                let id = request["id"].clone();
                let method = request["method"].as_str().unwrap_or_default().to_string();
                let now = head.load(Ordering::SeqCst);
                let result = match method.as_str() {
                    "eth_chainId" => json!("0x7a69"),
                    "eth_blockNumber" => json!(format!("{now:#x}")),
                    "eth_getBlockByNumber" => {
                        let number = request["params"][0]
                            .as_str()
                            .and_then(|tag| tag.strip_prefix("0x"))
                            .and_then(|hex| u64::from_str_radix(hex, 16).ok())
                            .unwrap_or(now);
                        let mut block =
                            alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                        block.header.inner.number = number;
                        block.header.inner.timestamp = time_of(number);
                        serde_json::to_value(block).unwrap()
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
                        if let Some(at) = at
                            && data.starts_with(&selector::<
                                chain::bindings::vault::IVault::sharePriceCall,
                            >())
                        {
                            samples.lock().unwrap().push(at);
                        }
                        if data == selector::<chain::bindings::factory::IVaultFactory::vaultsCall>()
                        {
                            json!(format!(
                                "0x{}{}{}",
                                "0".repeat(62) + "20",
                                "0".repeat(63) + "1",
                                "0".repeat(24) + VAULT
                            ))
                        } else {
                            json!(format!("0x{}", "0".repeat(64 * 128)))
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
async fn each_reading_is_the_first_block_its_hour_falls_in() {
    let head = Arc::new(AtomicU64::new(QUIET + 100_000));
    let samples = Arc::new(std::sync::Mutex::new(Vec::new()));
    let node = format!("http://{}", a_node(head.clone(), samples.clone()).await);
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
        "shonin-indexer-first-block-of-the-hour-{}",
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
    for _ in 0..3 {
        indexer.refresh().await.unwrap();
    }
    let mut blocks = samples.lock().unwrap().clone();
    blocks.dedup();
    assert!(blocks.len() >= 2, "readings at {blocks:?}");
    for pair in blocks.windows(2) {
        let gap = time_of(pair[1]) - time_of(pair[0]);
        assert!(
            gap <= 3_600 + QUIET_GAP,
            "readings at blocks {} and {} are {:.1} days apart; an hourly record has a reading in every hour's first block",
            pair[0],
            pair[1],
            gap as f64 / 86_400.0
        );
    }
}
