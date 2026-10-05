//! A reading whose block's time could not be read is not taken then, and is
//! asked for again at the next refresh: never dated by a guess (the vault's
//! creation, say), which would put a point at today's price in the middle of
//! the record, the series going back in time, and the record's drawdown and
//! monthly returns computed over it.
//!
//! Here: hour-long blocks (the local devnet's clock), a vault sixty days old,
//! and a node that fails one past block's header the first time it is asked
//! for it (a provider's rate limit).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::routing::post;
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;
use serde_json::{Value, json};

const DEPLOYED: u64 = 1_200_000;
const HEAD: u64 = DEPLOYED + 12 * 24 * 5;
const FAILS: u64 = DEPLOYED + 12 * 24 * 3;
const T0: u64 = 1_790_000_000;
const VAULT: &str = "00000000000000000000000000000000000000be";

/// Whether the failing block's header has been asked for yet.
static FAILED: AtomicBool = AtomicBool::new(false);

fn time_of(block: u64) -> u64 {
    T0 + 3_600 * (block - DEPLOYED)
}

fn selector<C: alloy::sol_types::SolCall>() -> String {
    format!("0x{}", alloy::hex::encode(C::SELECTOR))
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
                    let number = request["params"][0]
                        .as_str()
                        .and_then(|tag| tag.strip_prefix("0x"))
                        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
                        .unwrap_or(HEAD);
                    if number == FAILS && !FAILED.swap(true, Ordering::SeqCst) {
                        json!({"jsonrpc": "2.0", "id": id, "error": {
                            "code": 429, "message": "rate limited"
                        }})
                    } else {
                        let mut block =
                            alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                        block.header.inner.number = number;
                        block.header.inner.timestamp = time_of(number);
                        answer(serde_json::to_value(block).unwrap())
                    }
                }
                "eth_call" => {
                    let data = request["params"][0]["input"]
                        .as_str()
                        .or_else(|| request["params"][0]["data"].as_str())
                        .unwrap_or("0x")
                        .to_ascii_lowercase();
                    if data == selector::<chain::bindings::factory::IVaultFactory::vaultsCall>() {
                        answer(json!(format!(
                            "0x{}{}{}",
                            "0".repeat(62) + "20",
                            "0".repeat(63) + "1",
                            "0".repeat(24) + VAULT
                        )))
                    } else if data == selector::<chain::bindings::vault::IVault::createdAtCall>() {
                        answer(json!(format!("0x{T0:064x}")))
                    } else if data == selector::<chain::bindings::vault::IVault::sharePriceCall>() {
                        // $1.00 a share, as the asset counts it.
                        answer(json!(format!("0x{:064x}", 1_000_000u64)))
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
async fn a_sample_whose_block_time_is_unread_is_not_dated_at_the_vaults_creation() {
    let node = format!("http://{}", a_node().await);
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
    let store =
        std::env::temp_dir().join(format!("shonin-indexer-misdated-{}", std::process::id()));
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
    // Enough refreshes to read every hourly block since the vault's creation.
    for _ in 0..40 {
        indexer.refresh().await.unwrap();
    }

    assert!(
        FAILED.load(Ordering::SeqCst),
        "the failing header was asked for"
    );
    let snapshot = indexer.snapshot.read().await.clone();
    let strategy = snapshot.strategies.first().expect("the vault is read");
    let record = strategy.record.as_ref().expect("a record");
    let dates: Vec<_> = record
        .value_per_share
        .iter()
        .map(|point| point.date)
        .collect();
    assert!(
        dates.windows(2).all(|pair| pair[0] < pair[1]),
        "the record's days go back in time: {dates:?}"
    );
    // Every day from the vault's creation to the head's, the one whose
    // header failed included.
    let days = time_of(HEAD) / 86_400 - T0 / 86_400 + 1;
    assert_eq!(dates.len() as u64, days, "the record's days: {dates:?}");
}
