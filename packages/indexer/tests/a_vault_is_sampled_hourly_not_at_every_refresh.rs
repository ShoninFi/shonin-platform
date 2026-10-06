//! A vault's record is sampled hourly by chain time, not at every refresh:
//! a refresh runs every two seconds, and on a chain whose head moves
//! between refreshes, sampling whatever block is the head would take (and
//! keep) a reading every time, three reads each, for as long as the process
//! runs.
//!
//! Here: a chain with 2 s blocks, one vault, and 120 refreshes with the head
//! one block further each time (four minutes of chain time). Hourly sampling
//! takes one or two readings.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::routing::post;
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;
use serde_json::{Value, json};

const DEPLOYED: u64 = 5_000_001;
const VAULT: &str = "00000000000000000000000000000000000000be";
const GENESIS_TIME: u64 = 1_700_000_000;

fn selector<C: alloy::sol_types::SolCall>() -> String {
    format!("0x{}", alloy::hex::encode(C::SELECTOR))
}

/// A node whose head is `head`, with 2 s blocks. Counts `sharePrice` reads
/// asked of a past block: one per sample.
async fn a_node(head: Arc<AtomicU64>, samples: Arc<AtomicU64>) -> SocketAddr {
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
                        block.header.inner.timestamp = GENESIS_TIME + 2 * number;
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
                        if data.starts_with(&selector::<
                            chain::bindings::vault::IVault::sharePriceCall,
                        >()) && at.is_some()
                        {
                            samples.fetch_add(1, Ordering::SeqCst);
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
async fn a_vault_is_sampled_hourly_not_at_every_refresh() {
    let head = Arc::new(AtomicU64::new(DEPLOYED));
    let samples = Arc::new(AtomicU64::new(0));
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
    let store =
        std::env::temp_dir().join(format!("shonin-indexer-sampling-{}", std::process::id()));
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

    const REFRESHES: u64 = 120;
    for _ in 0..REFRESHES {
        indexer.refresh().await.unwrap();
        head.fetch_add(1, Ordering::SeqCst);
    }
    let taken = samples.load(Ordering::SeqCst);
    // Four minutes of chain time: hourly sampling takes the first reading
    // and perhaps one more.
    assert!(
        taken <= 2,
        "{taken} samples taken (and kept) over {REFRESHES} refreshes spanning {} s of chain time",
        2 * REFRESHES
    );
}
