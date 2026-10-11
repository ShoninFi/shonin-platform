//! A claim first read paid keeps the share of the vault it closed even when
//! its `ClaimPaid` lies past what one refresh reads of the vault's log (25
//! pages of 2,000 blocks): until the payment is read, the claim's fraction is
//! not kept for good, and once it is, it is measured against the supply
//! before the payment.
//!
//! Here: the indexer starts with a vault whose claim (50 of 100 shares) was
//! paid at block 60,000, and deposits since have brought the supply to 150.
//! Measured against today's supply with its shares put back, it would read
//! 0.25, and if kept as that, would stay 0.25 once the log is read through.

mod stand_in;

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
const PAID_IN: u64 = 60_000;

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
        block_hash: Some(B256::repeat_byte(PAID_IN as u8)),
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
                    "eth_blockNumber" => answer(json!(if logged {
                        format!("{:#x}", PAID_IN + 10)
                    } else {
                        "0x1".to_string()
                    })),
                    "eth_getLogs" => {
                        let asks_the_vault = stand_in::asks_the_vault(&request);
                        let bound = |key: &str| {
                            u64::from_str_radix(
                                request["params"][0][key]
                                    .as_str()
                                    .unwrap_or("0x0")
                                    .trim_start_matches("0x"),
                                16,
                            )
                            .unwrap_or(0)
                        };
                        let covers = bound("fromBlock") <= PAID_IN && PAID_IN <= bound("toBlock");
                        answer(if logged && asks_the_vault && covers {
                            json!([the_payment()])
                        } else {
                            json!([])
                        })
                    }
                    "eth_getBlockByNumber" => {
                        let mut block =
                            alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                        let number = stand_in::requested_block(
                            &request,
                            if logged { PAID_IN + 10 } else { 1 },
                        );
                        block.header.inner.number = number;
                        block.header.hash = B256::repeat_byte(number as u8);
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
                let mut reply = reply;
                if method == "eth_getLogs"
                    && let Some(logs) = reply["result"].as_array_mut()
                {
                    let filter: alloy::rpc::types::Filter =
                        serde_json::from_value(request["params"][0].clone()).unwrap();
                    logs.retain(|log| {
                        filter.rpc_matches(
                            &serde_json::from_value::<alloy::rpc::types::Log>(log.clone()).unwrap(),
                        )
                    });
                }
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
        logs_rpc: None,
        log_range: None,
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
async fn a_claim_paid_past_the_first_refresh_s_log_read_keeps_the_fraction_it_closed() {
    let indexer = an_indexer(Arc::new(AtomicBool::new(true)), true, "paid-late").await;
    // The first refreshes read the factory's and the vault's logs only part
    // of the way; later ones read them through to the payment.
    for _ in 0..4 {
        indexer.refresh().await.unwrap();
    }
    let closed = fraction(&indexer).await;
    assert!(
        (closed - 0.5).abs() < 1e-9,
        "a claim for 50 of 100 shares, first read paid: {closed}"
    );
}
