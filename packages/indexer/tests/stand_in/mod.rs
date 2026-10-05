//! A stand-in node: one vault, every read answered with zeros unless the
//! test's own answer says otherwise.

#![allow(dead_code)]

use std::sync::Arc;

use alloy::primitives::{Address, B256, Log as Primitive};
use alloy::sol_types::{SolCall, SolEvent};
use axum::Router;
use axum::routing::post;
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;
use serde_json::{Value, json};

pub const DEPLOYED: u64 = 1;
pub const VAULT: &str = "00000000000000000000000000000000000000be";

pub fn vault() -> Address {
    format!("0x{VAULT}").parse().unwrap()
}

pub fn selector<C: SolCall>() -> String {
    format!("0x{}", alloy::hex::encode(C::SELECTOR))
}

/// What a request asks for: the call's input, lowercased, for `eth_call`.
pub fn input(request: &Value) -> String {
    request["params"][0]["input"]
        .as_str()
        .or_else(|| request["params"][0]["data"].as_str())
        .unwrap_or("0x")
        .to_ascii_lowercase()
}

/// Whether a log request is for the vault's own logs.
pub fn asks_the_vault(request: &Value) -> bool {
    request["params"][0]["address"]
        .as_str()
        .is_some_and(|address| address.to_lowercase().ends_with(VAULT))
}

/// A log of the vault's, in `block` at `index`.
pub fn logged<E: SolEvent>(event: E, block: u64, index: u64) -> Value {
    let log = alloy::rpc::types::Log {
        inner: Primitive {
            address: vault(),
            data: event.encode_log_data(),
        },
        block_hash: Some(B256::repeat_byte(block as u8)),
        block_number: Some(block),
        block_timestamp: Some(1_790_000_000 + block),
        transaction_hash: Some(B256::repeat_byte(0x40 + index as u8)),
        transaction_index: Some(0),
        log_index: Some(index),
        removed: false,
    };
    serde_json::to_value(log).unwrap()
}

/// A test's answer to a request: `Some(Ok(result))`, `Some(Err(message))`
/// for a JSON-RPC error, or `None` for the default.
pub type Answer = Arc<dyn Fn(&Value) -> Option<Result<Value, String>> + Send + Sync>;

pub async fn a_node(answer: Answer) -> String {
    let app =
        Router::new().route(
            "/",
            post(move |axum::Json(request): axum::Json<Value>| {
                let answer = answer.clone();
                async move {
                    let id = request["id"].clone();
                    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
                    let reply = match answer(&request) {
                        Some(Ok(result)) => ok(result),
                        Some(Err(message)) => json!({
                            "jsonrpc": "2.0",
                            "id": request["id"].clone(),
                            "error": {"code": -32000, "message": message}
                        }),
                        None => {
                            let method = request["method"].as_str().unwrap_or_default();
                            match method {
                                "eth_chainId" => ok(json!("0x7a69")),
                                "eth_blockNumber" => ok(json!("0x10")),
                                "eth_getBlockByNumber" => {
                                    let mut block = alloy::rpc::types::Block::<
                                        alloy::rpc::types::Transaction,
                                    >::default(
                                    );
                                    block.header.inner.number = 0x10;
                                    block.header.inner.timestamp = 1_790_000_000;
                                    ok(serde_json::to_value(block).unwrap())
                                }
                                "eth_call" => {
                                    let data = input(&request);
                                    if data
                                        == selector::<
                                            chain::bindings::factory::IVaultFactory::vaultsCall,
                                        >()
                                    {
                                        ok(json!(format!(
                                            "0x{}{}{}",
                                            "0".repeat(62) + "20",
                                            "0".repeat(63) + "1",
                                            "0".repeat(24) + VAULT
                                        )))
                                    } else {
                                        ok(json!(format!("0x{}", "0".repeat(64 * 128))))
                                    }
                                }
                                _ => ok(json!([])),
                            }
                        }
                    };
                    axum::Json(reply)
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

pub fn deployments() -> Deployments {
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

pub async fn an_indexer(answer: Answer, name: &str) -> Arc<Indexer> {
    let node = a_node(answer).await;
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
        "shonin-indexer-stand-in-{name}-{}",
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
    indexer
}
