//! A vault's value-per-share record is sampled from the block the vault was
//! created in (its factory's `Activated` log, or the platform's deployment
//! when that can't be read), never from block 1, and every block asked
//! counts towards a refresh's cap whether or not it answers.
//!
//! Sampled from block 1, a vault's first refresh on a chain whose head is far
//! past it (every testnet and mainnet) makes one failing `eth_call` per twelve
//! blocks of the chain's whole history, and the chain's part of the record
//! is not written until it ends.
//!
//! Here: the platform was deployed at block 1,999,000 of a 2,000,000-block
//! chain, and the one vault was created there; the node answers no logs. A
//! refresh is given 20 s.

mod stand_in;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use chain::{ChainConfig, ChainKey, Deployments, Served};
use indexer::Indexer;
use serde_json::{Value, json};

const HEAD: u64 = 2_000_000;
const DEPLOYED: u64 = 1_999_000;
const VAULT: &str = "00000000000000000000000000000000000000be";

/// A node whose contracts exist from `DEPLOYED` on: a read at an earlier
/// block finds no code and returns nothing. Counts reads asked of blocks
/// before the deployment.
async fn a_node(before_deployment: Arc<AtomicU64>) -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(move |axum::Json(request): axum::Json<Value>| {
            let before_deployment = before_deployment.clone();
            async move {
                let id = request["id"].clone();
                if let Some(result) = stand_in::ancestry_answer(&request, |_| 1_700_000_000) {
                    return axum::Json(json!({"jsonrpc":"2.0","id":id,"result":result}));
                }
                let request = stand_in::numbered_data_request(&request);
                let method = request["method"].as_str().unwrap_or_default().to_string();
                let result = match method.as_str() {
                    "eth_chainId" => json!("0x7a69"),
                    "eth_blockNumber" => json!(format!("{HEAD:#x}")),
                    "eth_getBlockByNumber" => {
                        let mut block =
                            alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                        let number = stand_in::requested_block(&request, HEAD);
                        block.header.inner.number = number;
                        block.header.inner.parent_hash = if number == 0 {
                            alloy::primitives::B256::ZERO
                        } else {
                            stand_in::block_hash(number - 1)
                        };
                        block.header.inner.logs_bloom = alloy::primitives::Bloom::from([255; 256]);
                        block.header.hash = stand_in::block_hash(number);
                        block.header.inner.timestamp = 1_700_000_000;
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
                        let vaults = format!(
                            "0x{}",
                            alloy::hex::encode(
                                <chain::bindings::factory::IVaultFactory::vaultsCall
                                    as alloy::sol_types::SolCall>::SELECTOR
                            )
                        );
                        if at.is_some_and(|block| block < DEPLOYED) {
                            before_deployment.fetch_add(1, Ordering::SeqCst);
                            json!("0x")
                        } else if data == vaults {
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
async fn a_vault_s_record_is_read_from_the_deployment_not_from_block_one() {
    let before_deployment = Arc::new(AtomicU64::new(0));
    let node = format!("http://{}", a_node(before_deployment.clone()).await);
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
    let client = stand_in::client_with_limits(
        &config,
        &[("ancestry_reads", 4096), ("ancestry_cache_headers", 65536)],
    )
    .await;
    let store = std::env::temp_dir().join(format!(
        "shonin-sample-from-deployment-{}",
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

    let finished = tokio::time::timeout(Duration::from_secs(20), indexer.refresh())
        .await
        .is_ok();
    let asked_before = before_deployment.load(Ordering::SeqCst);
    // In the record, and read whole: not merely listed as not read yet.
    let in_record = indexer
        .snapshot
        .read()
        .await
        .strategies
        .iter()
        .any(|strategy| strategy.stale_as_of.is_none());
    assert!(
        finished && in_record && asked_before == 0,
        "refresh finished in 20 s: {finished}; vault in the record: {in_record}; \
         reads asked of blocks before the deployment so far: {asked_before} \
         (of about {} the first refresh would make)",
        DEPLOYED / 12
    );
}
