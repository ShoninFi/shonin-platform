//! What a served round promises about the order of its sends and about the
//! logs its decisions read, checked with the round's own reads.

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

/// An observation of every read the keeper has made on the chain, answered
/// by the node at its head (as `tests/served.rs` makes one).
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

/// A round's reads answered from `snapshot`, with the chain's logs from
/// `logs`.
fn served(
    client: &Observed,
    learned: &observed::Shared,
    snapshot: &Arc<Snapshot>,
    logs: Option<Arc<observed::Logs>>,
) -> Observed {
    learned.lock().unwrap().observed(snapshot);
    Observed::served(
        client.deref().clone(),
        learned.clone(),
        snapshot.clone(),
        logs,
    )
}

// ---------------------------------------------------------------------------
// The order of a served round's sends.
// ---------------------------------------------------------------------------

/// A killed vault past its halt's grace, its station stopped and empty,
/// nothing owed and no claims: a valuation in the slot lets it close, and
/// `close` itself has no window.
fn a_halted_vault_ready_to_close(node: &Shared, vault: Address, station: Address, now: u64) {
    answers(node, vault, IVault::stateCall {}, VaultState::Killed);
    answers(node, vault, IVault::timersCall {}, timers());
    // Collected after the grace, nothing carried: nothing left to bill.
    answers(node, vault, IVault::tankCall {}, a_tank(now, 0, now));
    answers(node, vault, IStationVault::haltedAtCall {}, now - 2 * 600);
    answers(node, vault, IStationVault::haltGraceCall {}, 600);
    answers(node, vault, IVault::homeStationCall {}, station);
    answers(node, station, IStation::specCall {}, a_spec(vec![]));
    answers(node, vault, IVault::valuationCountCall {}, U256::ZERO);
    answers(node, vault, IStationVault::unsettledCall {}, false);
    answers(node, vault, IVault::claimCountCall {}, U256::ZERO);
    answers(node, station, IStation::stoppedCall {}, true);
    answers(
        node,
        station,
        IStation::valueCall {},
        IStation::valueReturn {
            mark: U256::ZERO,
            exit: U256::ZERO,
        },
    );
    answers(
        node,
        station,
        IStation::unwindCall {
            fraction: U256::from(WAD),
        },
        IStation::unwindReturn {
            sentHome: U256::ZERO,
            blocked: false,
        },
    );
    answers(node, vault, IVault::closeCall {}, IVault::closeReturn {});
}

/// An open vault never valued, with a request waiting for its price.
fn a_running_vault_wanting_a_valuation(node: &Shared, vault: Address, station: Address, now: u64) {
    answers(node, vault, IVault::stateCall {}, VaultState::Open);
    answers(node, vault, IVault::timersCall {}, timers());
    answers(node, vault, IVault::tankCall {}, a_tank(now, 0, now));
    answers(node, vault, IStationVault::haltedAtCall {}, 0);
    answers(node, vault, IVault::homeStationCall {}, station);
    answers(node, station, IStation::specCall {}, a_spec(vec![]));
    answers(node, vault, IVault::valuationCountCall {}, U256::ZERO);
    answers(node, vault, IStationVault::unsettledCall {}, false);
    answers(node, station, IStation::outsideBandCall {}, false);
    answers(node, vault, IVault::claimCountCall {}, U256::ZERO);
}

/// A running vault, first in the factory's list, and a halted vault past its
/// grace, both wanting a valuation in the same slot, on a chain of
/// two-second blocks with a three-second window whose head is two seconds
/// before the slot: one round read from an observation. Every send taken
/// moves the chain on a block. What was sent, with the chain time each was
/// sent at.
async fn a_halted_and_a_running_vault_one_served_round() -> (Address, Address, Vec<(String, u64)>) {
    let halted = Address::repeat_byte(0x11);
    let running = Address::repeat_byte(0x12);
    let slot = SLOT_START + 10;
    let start = slot - 2;
    let node = stand_in::node(start);
    a_halted_vault_ready_to_close(&node, halted, Address::repeat_byte(0x21), start);
    a_running_vault_wanting_a_valuation(&node, running, Address::repeat_byte(0x22), start);
    let factory = deployments().factory;
    answers(
        &node,
        factory,
        IVaultFactory::vaultsCall {},
        vec![running, halted],
    );
    answers(
        &node,
        factory,
        IReplacesFactory::previousCall {},
        Address::ZERO,
    );
    stand_in::keeps_the_window(&node, 10, 3);
    let client = connect_with_blocks_of(&node, 2).await;
    let learned = observed::Shared::default();
    let mut memory = HashMap::new();
    let mut factories = Factories::default();
    // A first round of the chain itself learns what the rounds read, at a
    // moment no valuation lands in a window.
    node.lock().unwrap().timestamp = start - 5;
    let live = Observed::live(client.deref().clone(), learned.clone());
    let _ = tick(
        Chain::Base,
        &live,
        &deployments(),
        KEEPER,
        "http://127.0.0.1:1",
        &mut memory,
        &mut factories,
    )
    .await;
    {
        let mut node = node.lock().unwrap();
        node.timestamp = start;
        node.head += 3;
    }
    let before = stand_in::everything_sent_when(&node).len();
    let (_, snapshot) = look(&client, &learned, &node).await;
    let reads = served(&client, &learned, &snapshot, None);
    let _ = tick(
        Chain::Base,
        &reads,
        &deployments(),
        KEEPER,
        "http://127.0.0.1:1",
        &mut memory,
        &mut factories,
    )
    .await;
    let sent = stand_in::everything_sent_when(&node)[before..].to_vec();
    (halted, running, sent)
}

/// A running vault that wants a valuation and a halted vault past its
/// grace, its station emptied, in one served round: the running vault's
/// valuation lands inside its window, and the halted vault's close, which
/// has no window, never takes a block ahead of it. (Sends wait for each
/// other's receipts, so a three-second window at two-second blocks holds one
/// valuation that may land a block late: the halted vault's waits for the
/// next slot.)
#[tokio::test]
async fn a_halted_vault_s_close_does_not_take_the_block_another_vault_s_valuation_needs() {
    let (halted, running, sent) = a_halted_and_a_running_vault_one_served_round().await;
    let block = 2;
    let valued = alloy::hex::encode_prefixed(IVault::recordValuationCall::SELECTOR);
    let closed = alloy::hex::encode_prefixed(IVault::closeCall::SELECTOR);
    let sends_of = |vault: Address, selector: &str| -> Vec<u64> {
        sent.iter()
            .filter(|(call, _)| call.starts_with(&format!("{vault}")) && call.ends_with(selector))
            .map(|(_, at)| *at)
            .collect()
    };
    let in_window = |at: u64| (at + block) % 10 <= 3;
    let halted_closed = sends_of(halted, &closed);
    let running_valued = sends_of(running, &valued);
    assert_eq!(
        running_valued.len(),
        1,
        "the running vault's valuation was not sent in the round: {sent:?}"
    );
    assert!(
        running_valued.iter().all(|at| in_window(*at)),
        "the running vault's valuation lands outside its window: {sent:?}"
    );
    assert!(
        halted_closed.iter().all(|at| *at > running_valued[0]),
        "the halted vault's close went ahead of the running vault's valuation: {sent:?}"
    );
}

// ---------------------------------------------------------------------------
// The logs a served round's decisions read, from the round's own reads.
// ---------------------------------------------------------------------------

/// A `ProRataTaken` log `vault` wrote for `owner` in block `block`, as a
/// node lists it .
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
/// everything else is the stand-in's .
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

/// An open vault with a priced valuation and nothing waiting, between
/// slots: its round sends nothing.
fn an_open_vault_with_nothing_due(node: &Shared, now: u64) {
    answers(node, VAULT, IVault::stateCall {}, VaultState::Open);
    answers(node, VAULT, IVault::timersCall {}, timers());
    answers(node, VAULT, IVault::tankCall {}, a_tank(now, 0, now));
    answers(node, VAULT, IStationVault::haltedAtCall {}, 0);
    answers(node, VAULT, IVault::homeStationCall {}, STATION);
    answers(node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    a_quiet_vault(node, now - 10);
}

/// A holder takes units in block 20. The keeper's own round has read the
/// vault of the chain itself (so every look holds what the round reads),
/// and the looks at blocks 19 to 22 follow, each read on the log reader as
/// the chain's loop does. Whether the holder is known by the look after the
/// one taken at block 20.
async fn holder_found_at_the_next_look() {
    let node = stand_in::node(BETWEEN_SLOTS);
    let holder = Address::repeat_byte(0x44);
    let taken_in = 20u64;
    an_open_vault_with_nothing_due(&node, BETWEEN_SLOTS);
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
    reader.watch(vec![VAULT, STATION]);
    let learned = observed::Shared::default();
    // Its daily round is done: the round sends nothing.
    let mut memory = Memory {
        daily_at: Some(BETWEEN_SLOTS),
        ..Memory::default()
    };
    // The keeper's first round reads the vault of the chain itself: what it
    // reads is what every look holds after.
    let live = Observed::live(client.deref().clone(), learned.clone());
    keep_one(
        &live,
        KEEPER,
        &deployments(),
        "http://127.0.0.1:1",
        VAULT,
        &mut memory,
    )
    .await
    .unwrap();
    assert_eq!(
        live.sends(),
        0,
        "the round sent: {:?}",
        everything_sent(&node)
    );
    let mut previous: Option<Arc<Snapshot>> = None;
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
        let reads = served(&client, &learned, &snapshot, reader.view());
        find_holders(&reads, VAULT, 1, &mut memory).await.unwrap();
        previous = Some(snapshot);
        if head > taken_in {
            assert!(
                memory.holders.contains(&holder),
                "at the look taken at block {head}, the holder who took units in block {taken_in} is not known: \
                 the log reader has read through block {} and was {}; the round's reads of the vault are {:?}",
                reads.log_head().await.unwrap(),
                if matches!(scanned, Scanned::Idle) {
                    "not due to read on"
                } else {
                    "read on"
                },
                learned
                    .lock()
                    .unwrap()
                    .plan(Instant::now())
                    .reads
                    .iter()
                    .filter(|read| read.target == VAULT)
                    .map(|read| alloy::hex::encode_prefixed(&read.data[..4]))
                    .collect::<Vec<_>>()
            );
        }
    }
}

/// An open vault whose round sends nothing: its own reads hold the vault's
/// units total, so the look after a holder takes units shows it changed,
/// the log reader reads on, and the holder is found at the next look rather
/// than at the reader's backstop.
#[tokio::test]
async fn a_holder_who_took_units_is_found_at_the_next_look_from_the_round_s_own_reads() {
    holder_found_at_the_next_look().await;
}
