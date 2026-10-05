//! A paid claim keeps the share of the vault it closed. Paying a claim burns
//! its shares while the claim keeps its `shares`, so the supply after the
//! payment no longer counts them: measured against it, a claim for half the
//! vault would read as having closed all of it. The fraction is measured
//! against the supply before the payment (the block before its `ClaimPaid`,
//! or the share it stood for when last read waiting) and kept.
//!
//! Here: a claim for 50 of 100 shares, read once while it waits (0.5), then
//! paid (the supply now 50); and one first read already paid, its payment
//! in the vault's log, with deposits since bringing the supply to 150.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alloy::primitives::{Address, B256, Log as Primitive, U256};
use alloy::sol_types::{SolCall, SolEvent};
use axum::Router;
use axum::routing::post;
use chain::bindings::vault::{Claim, ClaimState, IVault};
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;
use serde_json::{Value, json};

const DEPLOYED: u64 = 1;
const VAULT: &str = "00000000000000000000000000000000000000be";

fn selector<C: SolCall>() -> String {
    format!("0x{}", alloy::hex::encode(C::SELECTOR))
}

fn shares(count: u64) -> U256 {
    U256::from(count) * U256::from(10u64.pow(18))
}

/// The block the claim was paid in, when the vault's log shows it.
const PAID_IN: u64 = 5;

/// The vault's `ClaimPaid` for claim 1, in block [`PAID_IN`].
fn the_payment() -> Value {
    let vault: Address = format!("0x{VAULT}").parse().unwrap();
    let event = IVault::ClaimPaid {
        claimId: U256::from(1),
        assets: U256::from(50_000_000u64),
    };
    let log = alloy::rpc::types::Log {
        inner: Primitive {
            address: vault,
            data: event.encode_log_data(),
        },
        block_hash: Some(B256::repeat_byte(1)),
        block_number: Some(PAID_IN),
        block_timestamp: Some(1_790_000_000),
        transaction_hash: Some(B256::repeat_byte(2)),
        transaction_index: Some(0),
        log_index: Some(0),
        removed: false,
    };
    serde_json::to_value(log).unwrap()
}

/// A node holding one claim for 50 of 100 shares, paid once `paid` is set.
/// With `logged`, the head is past [`PAID_IN`], the vault's log shows the
/// payment there, the supply read at the block before counts the claim's
/// shares, and deposits since have brought the supply to 150.
async fn a_node(paid: Arc<AtomicBool>, logged: bool) -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(move |axum::Json(request): axum::Json<Value>| {
            let paid = paid.load(Ordering::SeqCst);
            async move {
                let id = request["id"].clone();
                let method = request["method"].as_str().unwrap_or_default().to_string();
                let answer = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
                let reply = match method.as_str() {
                    "eth_chainId" => answer(json!("0x7a69")),
                    "eth_blockNumber" => answer(json!(if logged { "0x10" } else { "0x1" })),
                    "eth_getLogs" => {
                        let asks_the_vault = request["params"][0]["address"]
                            .as_str()
                            .is_some_and(|address| address.to_lowercase().ends_with(VAULT));
                        answer(if logged && asks_the_vault {
                            json!([the_payment()])
                        } else {
                            json!([])
                        })
                    }
                    "eth_getBlockByNumber" => {
                        let mut block =
                            alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                        block.header.inner.number = if logged { 0x10 } else { 1 };
                        block.header.inner.timestamp = 1_790_000_000;
                        answer(serde_json::to_value(block).unwrap())
                    }
                    "eth_call" => {
                        let data = request["params"][0]["input"]
                            .as_str()
                            .or_else(|| request["params"][0]["data"].as_str())
                            .unwrap_or("0x")
                            .to_ascii_lowercase();
                        let word = |value: U256| answer(json!(format!("0x{value:064x}")));
                        if data == selector::<chain::bindings::factory::IVaultFactory::vaultsCall>()
                        {
                            answer(json!(format!(
                                "0x{}{}{}",
                                "0".repeat(62) + "20",
                                "0".repeat(63) + "1",
                                "0".repeat(24) + VAULT
                            )))
                        } else if data == selector::<IVault::claimCountCall>() {
                            word(U256::from(1))
                        } else if data == selector::<IVault::totalSupplyCall>() {
                            // Paying the claim burned its 50 shares.
                            let before_payment = request["params"][1].as_str()
                                == Some(&format!("{:#x}", PAID_IN - 1));
                            word(if !paid || before_payment {
                                shares(100)
                            } else if logged {
                                // And deposits since have brought it to 150.
                                shares(150)
                            } else {
                                shares(50)
                            })
                        } else if data.starts_with(&selector::<IVault::claimAtCall>()) {
                            let claim = Claim {
                                owner: Address::repeat_byte(0x22),
                                shares: shares(50),
                                bookedAt: 1_790_000_000,
                                lastAttemptAt: 1_790_000_000,
                                state: if paid {
                                    ClaimState::Paid
                                } else {
                                    ClaimState::Pending
                                },
                                paid: if paid {
                                    U256::from(50_000_000u64)
                                } else {
                                    U256::ZERO
                                },
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

async fn an_indexer(paid: Arc<AtomicBool>, logged: bool, name: &str) -> Arc<Indexer> {
    let node = format!("http://{}", a_node(paid, logged).await);
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
    indexer
}

async fn fraction(indexer: &Indexer) -> f64 {
    indexer.snapshot.read().await.strategies[0].vault.claims[0].fraction_closed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paid_claim_keeps_the_fraction_it_closed() {
    let paid = Arc::new(AtomicBool::new(false));
    let indexer = an_indexer(paid.clone(), false, "paid-claim").await;
    indexer.refresh().await.unwrap();
    let waiting = fraction(&indexer).await;
    paid.store(true, Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    let after = fraction(&indexer).await;
    assert!(
        (waiting - 0.5).abs() < 1e-9 && (after - 0.5).abs() < 1e-9,
        "a claim for 50 of 100 shares: {waiting} while waiting, {after} once paid"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_first_read_paid_is_measured_against_the_supply_before_its_payment() {
    let indexer = an_indexer(Arc::new(AtomicBool::new(true)), true, "paid-in-the-log").await;
    indexer.refresh().await.unwrap();
    let closed = fraction(&indexer).await;
    assert!(
        (closed - 0.5).abs() < 1e-9,
        "a claim for 50 of 100 shares, first read paid: {closed}"
    );
}
