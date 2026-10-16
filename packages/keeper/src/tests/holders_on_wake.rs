//! What the chain's loop promises about logs on the served path: a log
//! written in the block a look was taken at is read by a later look, not
//! left to the log reader's backstop.

use std::ops::Deref;
use std::sync::Arc;
use std::time::Instant;

use alloy::network::TransactionBuilder;
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::sol_types::{SolEvent, SolValue};
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use chain::follow::{self, Follower, Item, Path};
use chain::rpc_policy::RpcPolicy;
use serde_json::{Value, json};

use super::*;
use crate::logs::{LogReader, Scanned};
use crate::observed::{self, Snapshot};
use crate::stand_in::{self, Shared};

/// A `ProRataTaken` log `vault` wrote for `owner` in block `block`, as a
/// node lists it: the block's hash is the stand-in's (zero), so the log
/// checks against the stand-in's headers.
fn pro_rata_taken(vault: Address, owner: Address, units: u64, block: u64) -> Value {
    let data = (U256::from(1u64), U256::from(1u64), U256::from(units)).abi_encode();
    json!({
        "address": format!("{vault:?}"),
        "topics": [
            format!("{:?}", IVault::ProRataTaken::SIGNATURE_HASH),
            format!("{:?}", B256::left_padding_from(owner.as_slice())),
        ],
        "data": alloy::hex::encode_prefixed(data),
        "blockNumber": format!("0x{block:x}"),
        "blockHash": format!("{:?}", B256::ZERO),
        "transactionHash": format!("{:?}", B256::repeat_byte(0xdd)),
        "transactionIndex": "0x0",
        "logIndex": "0x0",
        "removed": false,
    })
}

type LogState = Arc<(reqwest::Client, String, Address, Value)>;

/// A client whose node lists `log` for `address` in the block the log names,
/// and only when a log read's range holds that block, as a node does;
/// everything else is the stand-in's.
async fn connect_with_a_log(node: &Shared, address: Address, log: Value) -> Observed {
    let upstream = stand_in::connect(node).await.rpc_url.clone();
    let state: LogState = Arc::new((reqwest::Client::new(), upstream, address, log));
    async fn one(state: &LogState, asked: &Value) -> Value {
        let (http, upstream, address, log) = &**state;
        if asked["method"] == "eth_getLogs" {
            let params = &asked["params"][0];
            let wanted = format!("{address:?}").to_lowercase();
            let names = |one: &Value| one.as_str().is_some_and(|one| one.to_lowercase() == wanted);
            let named = match &params["address"] {
                Value::Array(many) => many.iter().any(names),
                one => names(one),
            };
            let block = |value: &Value| {
                value
                    .as_str()
                    .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
                    .unwrap_or_default()
            };
            if named {
                let at = block(&log["blockNumber"]);
                let listed = if at >= block(&params["fromBlock"]) && at <= block(&params["toBlock"])
                {
                    vec![log.clone()]
                } else {
                    Vec::new()
                };
                return json!({"jsonrpc": "2.0", "id": asked["id"], "result": listed});
            }
        }
        http.post(upstream)
            .json(asked)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    let app = Router::new()
        .route(
            "/",
            post(
                |State(state): State<LogState>, Json(body): Json<Value>| async move {
                    match body {
                        Value::Array(batch) => {
                            let mut answered = Vec::new();
                            for asked in &batch {
                                answered.push(one(&state, asked).await);
                            }
                            Json(Value::Array(answered))
                        }
                        asked => Json(one(&state, &asked).await),
                    }
                },
            ),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let config = chain::ChainConfig {
        label: "stand-in with a log".to_string(),
        rpc: url.clone(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        confirmations: None,
        public_rpc: url,
        logs_rpc: None,
        log_range: None,
    };
    Client::connect(&config, chain::Signing::ReadOnly)
        .await
        .unwrap()
        .into()
}

/// One look at the chain: an observation at the node's head of every read
/// learned so far, answered by the node, as the chain's loop gets one from
/// the follower.
async fn look(
    client: &Observed,
    learned: &observed::Shared,
    node: &Shared,
) -> (follow::Observed, Arc<Snapshot>) {
    let plan = learned.lock().unwrap().plan(Instant::now());
    let mut items = Vec::new();
    for read in &plan.reads {
        let request = TransactionRequest::default()
            .with_to(read.target)
            .with_input(read.data.clone());
        items.push(match client.provider().call(request).await {
            Ok(answer) => Item::Returned(answer),
            Err(_) => Item::Reverted(Bytes::new()),
        });
    }
    let (number, timestamp) = {
        let node = node.lock().unwrap();
        (node.head, node.timestamp)
    };
    let observed = follow::Observed {
        endpoint: 0,
        path: Path::ViewReads,
        number,
        timestamp,
        parent_hash: B256::ZERO,
        hash: Some(B256::ZERO),
        hashes: Default::default(),
        zero: Vec::new(),
        balances: vec![U256::ZERO; plan.balances.len()],
        items,
        wall_ms: 0,
        gas_at_entry: None,
        requests: 1,
    };
    let snapshot = Arc::new(Snapshot::new(&plan, &observed));
    (observed, snapshot)
}

/// A holder who takes units in block `b` is found at the look after the
/// one taken at block `b`: that look shows the vault's units changed, the
/// log reader reads on up to the block below the look's own and stays
/// wanted, and the next look reads block `b`. The keeper's serve of those
/// units may then go out within its notice bound, rather than at the log
/// reader's backstop.
#[tokio::test]
async fn a_holder_who_took_units_in_the_block_a_look_was_taken_at_is_found_at_the_next_look() {
    let node = stand_in::node(BETWEEN_SLOTS);
    let holder = Address::repeat_byte(0x44);
    let taken_in = 20u64;
    let units_total = |total: u64| {
        answers(
            &node,
            VAULT,
            IVault::unitsCall {},
            IVault::unitsReturn {
                total: U256::from(total),
                reserve: U256::ZERO,
            },
        );
    };
    units_total(0);
    let client =
        connect_with_a_log(&node, VAULT, pro_rata_taken(VAULT, holder, 10, taken_in)).await;
    let policy = RpcPolicy::default();
    let mut follower = Follower::new(client.deref().clone(), policy.clone());
    let mut reader = LogReader::new(1, &policy);
    reader.watch(vec![VAULT]);
    let learned = observed::Shared::default();
    // The vault's units are among the keeper's reads (its first round read
    // them of the chain itself), so every look holds them.
    Observed::live(client.deref().clone(), learned.clone())
        .read(VAULT, IVault::unitsCall {})
        .await
        .unwrap();
    let mut memory = Memory::default();
    let mut previous: Option<Arc<Snapshot>> = None;
    // Looks at blocks 19, 20, 21 and 22; the holder takes units in block 20.
    for (head, total) in [(19u64, 0u64), (20, 10), (21, 10), (22, 10)] {
        {
            let mut node = node.lock().unwrap();
            node.head = head;
            node.timestamp += 2;
        }
        units_total(total);
        let (observed, snapshot) = look(&client, &learned, &node).await;
        // As the chain's loop does before every served round.
        want_logs(&mut reader, previous.as_deref(), &snapshot, observed.number);
        let scanned = reader.scan(&mut follower, &observed, Instant::now()).await;
        assert!(
            matches!(scanned, Scanned::Idle | Scanned::Read),
            "the logs were not taken at block {head}: {scanned:?}"
        );
        learned.lock().unwrap().observed(&snapshot);
        let reads = Observed::served(
            client.deref().clone(),
            learned.clone(),
            snapshot.clone(),
            reader.view(),
        );
        find_holders(&reads, VAULT, 1, &mut memory).await.unwrap();
        previous = Some(snapshot);
        if head > taken_in {
            assert!(
                memory.holders.contains(&holder),
                "at the look taken at block {head}, the holder who took units in block {taken_in} is not known: \
                 the log reader has read through block {} and was {}",
                reads.log_head().await.unwrap(),
                if matches!(scanned, Scanned::Idle) {
                    "not due to read on"
                } else {
                    "read on"
                }
            );
        }
    }
}
