//! A vault's own logs, and what is read of it alone, are read from the block
//! it was created in (its factory's `Activated` log), not from the
//! platform's deployment: a vault created long after the deployment would
//! otherwise have every block between asked for its logs before anything of
//! its own is read. A vault's kill is shown however many of its events
//! follow it, and two claims paid in one block each close the share of the
//! supply left when it was paid, in the order the block paid them.
//!
//! Here: the platform was deployed at block 1; the one vault was created at
//! block 40,000, the head is 40,100.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, B256, Log as Primitive, LogData, U256};
use alloy::sol_types::{SolCall, SolEvent};
use axum::Router;
use axum::routing::post;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::vault::{Claim, ClaimState, IVault};
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;
use serde_json::{Value, json};

const DEPLOYED: u64 = 1;
const CREATED_IN: u64 = 40_000;
const HEAD: u64 = 40_100;
/// The block both claims were paid in.
const PAID_IN: u64 = 40_050;
const VAULT: &str = "00000000000000000000000000000000000000be";

fn vault() -> Address {
    format!("0x{VAULT}").parse().unwrap()
}

fn selector<C: SolCall>() -> String {
    format!("0x{}", alloy::hex::encode(C::SELECTOR))
}

fn shares(count: u64) -> U256 {
    U256::from(count) * U256::from(10u64.pow(18))
}

fn a_log(address: Address, data: LogData, block: u64, log_index: u64) -> Value {
    let log = alloy::rpc::types::Log {
        inner: Primitive { address, data },
        block_hash: Some(B256::repeat_byte(1)),
        block_number: Some(block),
        block_timestamp: Some(1_790_000_000 + block),
        transaction_hash: Some(B256::repeat_byte(2)),
        transaction_index: Some(0),
        log_index: Some(log_index),
        removed: false,
    };
    serde_json::to_value(log).unwrap()
}

/// The vault's own logs: it was stopped, sixty-one deposits were priced
/// after that, and both its claims were paid in one block, claim 2 first.
fn the_vault_s_logs() -> Vec<Value> {
    let mut logs = vec![a_log(
        vault(),
        IVault::Halted {
            reason: B256::right_padding_from(b"killed"),
            haltedAt: 1_790_000_000,
        }
        .encode_log_data(),
        CREATED_IN + 10,
        0,
    )];
    for index in 0..61 {
        logs.push(a_log(
            vault(),
            IVault::Deposited {
                requestId: U256::from(index),
                owner: Address::repeat_byte(0x33),
                assets: U256::from(1_000_000u64),
                shares: shares(1),
            }
            .encode_log_data(),
            CREATED_IN + 20,
            index,
        ));
    }
    for (log_index, claim) in [(4, 2), (7, 1)] {
        logs.push(a_log(
            vault(),
            IVault::ClaimPaid {
                claimId: U256::from(claim),
                assets: U256::from(50_000_000u64),
            }
            .encode_log_data(),
            PAID_IN,
            log_index,
        ));
    }
    logs
}

/// A node whose factory created the vault at [`CREATED_IN`]. Keeps the first
/// block of every read of the vault's own logs.
async fn a_node(asked_from: Arc<Mutex<Vec<u64>>>) -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(move |axum::Json(request): axum::Json<Value>| {
            let asked_from = asked_from.clone();
            async move {
                let id = request["id"].clone();
                let method = request["method"].as_str().unwrap_or_default().to_string();
                let answer = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
                let reply = match method.as_str() {
                    "eth_chainId" => answer(json!("0x7a69")),
                    "eth_blockNumber" => answer(json!(format!("{HEAD:#x}"))),
                    "eth_getLogs" => {
                        let filter = &request["params"][0];
                        let bound = |key: &str| {
                            u64::from_str_radix(
                                filter[key]
                                    .as_str()
                                    .unwrap_or("0x0")
                                    .trim_start_matches("0x"),
                                16,
                            )
                            .unwrap_or(0)
                        };
                        let (from, to) = (bound("fromBlock"), bound("toBlock"));
                        let asks_the_vault = filter["address"]
                            .as_str()
                            .is_some_and(|address| address.to_lowercase().ends_with(VAULT));
                        let asks_for_activations = filter["topics"][0].as_str()
                            == Some(&format!("{:#x}", IVaultFactory::Activated::SIGNATURE_HASH));
                        let logs: Vec<Value> = if asks_the_vault {
                            asked_from.lock().unwrap().push(from);
                            the_vault_s_logs()
                        } else if asks_for_activations {
                            vec![a_log(
                                Address::with_last_byte(1),
                                IVaultFactory::Activated {
                                    strategyId: U256::from(1),
                                    author: Address::repeat_byte(0x22),
                                    vault: vault(),
                                    station: Address::ZERO,
                                    tankFunding: U256::ZERO,
                                }
                                .encode_log_data(),
                                CREATED_IN,
                                0,
                            )]
                        } else {
                            Vec::new()
                        };
                        let logs: Vec<Value> = logs
                            .into_iter()
                            .filter(|log| {
                                let block = u64::from_str_radix(
                                    log["blockNumber"]
                                        .as_str()
                                        .unwrap_or("0x0")
                                        .trim_start_matches("0x"),
                                    16,
                                )
                                .unwrap_or(0);
                                from <= block && block <= to
                            })
                            .collect();
                        answer(json!(logs))
                    }
                    "eth_getBlockByNumber" => {
                        let mut block =
                            alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                        block.header.inner.number = HEAD;
                        block.header.inner.timestamp = 1_790_000_000 + HEAD;
                        answer(serde_json::to_value(block).unwrap())
                    }
                    "eth_call" => {
                        let data = request["params"][0]["input"]
                            .as_str()
                            .or_else(|| request["params"][0]["data"].as_str())
                            .unwrap_or("0x")
                            .to_ascii_lowercase();
                        let word = |value: U256| answer(json!(format!("0x{value:064x}")));
                        if data == selector::<IVaultFactory::vaultsCall>() {
                            answer(json!(format!(
                                "0x{}{}{}",
                                "0".repeat(62) + "20",
                                "0".repeat(63) + "1",
                                "0".repeat(24) + VAULT
                            )))
                        } else if data == selector::<IVault::claimCountCall>() {
                            word(U256::from(2))
                        } else if data == selector::<IVault::totalSupplyCall>() {
                            // 200 before the block both claims were paid in,
                            // 100 once both had burned their 50.
                            let before = request["params"][1].as_str()
                                == Some(&format!("{:#x}", PAID_IN - 1));
                            word(if before { shares(200) } else { shares(100) })
                        } else if data.starts_with(&selector::<IVault::claimAtCall>()) {
                            let claim = Claim {
                                owner: Address::repeat_byte(0x22),
                                shares: shares(50),
                                bookedAt: 1_790_000_000,
                                lastAttemptAt: 1_790_000_000,
                                state: ClaimState::Paid,
                                paid: U256::from(50_000_000u64),
                            };
                            answer(json!(format!(
                                "0x{}",
                                alloy::hex::encode(IVault::claimAtCall::abi_encode_returns(&claim))
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

/// An indexer refreshed until it has read the chain through, and the first
/// block of every read of the vault's own logs.
async fn read_through(name: &str) -> (Arc<Indexer>, Vec<u64>) {
    let asked_from = Arc::new(Mutex::new(Vec::new()));
    let node = format!("http://{}", a_node(asked_from.clone()).await);
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
    let store = std::env::temp_dir().join(format!("shonin-indexer-{name}-{}", std::process::id()));
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
    let asked = asked_from.lock().unwrap().clone();
    (indexer, asked)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vault_s_own_logs_are_read_from_the_block_it_was_created_in() {
    let (_, asked) = read_through("from-creation").await;
    assert!(!asked.is_empty(), "the vault's logs were never read");
    assert!(
        asked.iter().all(|from| *from >= CREATED_IN),
        "the vault's logs were read from blocks {asked:?}; it was created in {CREATED_IN}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_kill_is_shown_however_many_events_follow_it() {
    let (indexer, _) = read_through("kill-shown").await;
    let snapshot = indexer.snapshot.read().await;
    assert!(
        snapshot.strategies[0].kill.is_some(),
        "a vault stopped sixty-one events ago shows no kill"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_claims_paid_in_one_block_each_close_the_share_left_when_it_was_paid() {
    let (indexer, _) = read_through("same-block-claims").await;
    let snapshot = indexer.snapshot.read().await;
    let claims = &snapshot.strategies[0].vault.claims;
    let (first, second) = (claims[0].fraction_closed, claims[1].fraction_closed);
    // Claim 2 was paid first, from 200 shares; claim 1 from the 150 left.
    assert!(
        (second - 0.25).abs() < 1e-9 && (first - 50.0 / 150.0).abs() < 1e-9,
        "claim 1 closed {first}, claim 2 closed {second}"
    );
}
