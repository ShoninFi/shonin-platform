//! The vault never trades: its home station is the venue account's
//! master, and the executor places every order with `master: station`, so
//! the venue's `Filled` names the station in its first topic. A vault's
//! trades are its station's fills: on its page, in its record's count, and
//! in the platform's total.
//!
//! Here: a node holding one fill of the vault's station.

use std::net::SocketAddr;
use std::sync::Arc;

use alloy::primitives::{Address, B256, Log as Primitive, U256};
use alloy::sol_types::{SolCall, SolEvent};
use axum::Router;
use axum::routing::post;
use chain::bindings::vault::IVault;
use chain::bindings::venue::ILocalVenue;
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;
use serde_json::{Value, json};

const DEPLOYED: u64 = 1;
const VAULT: &str = "00000000000000000000000000000000000000be";
const STATION: Address = Address::repeat_byte(0x5a);
const VENUE: Address = Address::with_last_byte(1);

fn selector<C: SolCall>() -> String {
    format!("0x{}", alloy::hex::encode(C::SELECTOR))
}

/// The one fill on the chain: the station's.
fn the_fill() -> Value {
    let event = ILocalVenue::Filled {
        master: STATION,
        market: B256::with_last_byte(7),
        by: Address::repeat_byte(0xee),
        isBuy: true,
        reduceOnly: false,
        size: U256::from(10u64.pow(18)),
        price: U256::from(100_000_000u64),
        notional: U256::from(100_000_000u64),
    };
    let log = alloy::rpc::types::Log {
        inner: Primitive {
            address: VENUE,
            data: event.encode_log_data(),
        },
        block_hash: Some(B256::repeat_byte(1)),
        block_number: Some(1),
        block_timestamp: Some(1_790_000_000),
        transaction_hash: Some(B256::repeat_byte(2)),
        transaction_index: Some(0),
        log_index: Some(0),
        removed: false,
    };
    serde_json::to_value(log).unwrap()
}

/// Whether a log matches a request's address and topics, as a node judges.
fn matches(filter: &Value, log: &Value) -> bool {
    let same = |a: &Value, b: &Value| {
        a.as_str().map(str::to_lowercase) == b.as_str().map(str::to_lowercase)
    };
    let one_of = |wanted: &Value, value: &Value| match wanted {
        Value::Null => true,
        Value::Array(any) => any.iter().any(|w| same(w, value)),
        _ => same(wanted, value),
    };
    if !one_of(&filter["address"], &log["address"]) {
        return false;
    }
    let topics = log["topics"].as_array().unwrap();
    filter["topics"]
        .as_array()
        .map(|wanted| {
            wanted.iter().enumerate().all(|(i, wanted)| {
                wanted.is_null() || topics.get(i).is_some_and(|topic| one_of(wanted, topic))
            })
        })
        .unwrap_or(true)
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
                "eth_blockNumber" => answer(json!("0x1")),
                "eth_getBlockByNumber" => {
                    let mut block =
                        alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                    block.header.inner.number = 1;
                    block.header.inner.timestamp = 1_790_000_000;
                    answer(serde_json::to_value(block).unwrap())
                }
                "eth_getLogs" => {
                    let fill = the_fill();
                    let filter = &request["params"][0];
                    answer(if matches(filter, &fill) {
                        json!([fill])
                    } else {
                        json!([])
                    })
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
                    } else if data == selector::<IVault::homeStationCall>() {
                        answer(json!(format!("0x{:0>64}", alloy::hex::encode(STATION))))
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
async fn the_vault_s_trades_are_its_station_s_fills() {
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
    let store = std::env::temp_dir().join(format!("shonin-indexer-trades-{}", std::process::id()));
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
    indexer.refresh().await.unwrap();

    let snapshot = indexer.snapshot.read().await.clone();
    let strategy = snapshot.strategies.first().expect("the vault is read");
    assert_eq!(
        strategy.vault.station.to_lowercase(),
        format!("{STATION:?}").to_lowercase(),
        "the stand-in names the station"
    );
    assert!(
        strategy.trades.len() == 1 && snapshot.totals.trades == 1,
        "the station filled one order; the page shows {} trades, the totals {}",
        strategy.trades.len(),
        snapshot.totals.trades
    );
}
