//! A closed vault in its last bill's window (fifteen minutes past its
//! close; the keeper's own clock is let off here, so the window is judged on
//! the chain's). The executor answers with a total, and every `keep` carrying
//! it passes its estimate and is mined and reverts for the first thirteen
//! minutes of the window (whatever it was, it has passed by then). So the
//! keeper waits between `keep`s, a slot, then two, four and so on. Near the
//! window's end the agent's last orders have landed and the executor answers
//! with a newer total.
//!
//! The newer total has never been sent, so the vault has refused nothing of
//! it: the older total's wait does not hold it back, and it is sent before
//! the vault goes quiet (looked at once a day), since the author of a closed
//! vault may take the tank at once.

mod process;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use axum::Json;
use axum::extract::State;
use axum::routing::post;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    DAY, STATION, Shared, VAULT, a_spec, answers, node, one_vault, serve, serve_node,
    start_keeper_holding, timers, wait_until,
};
use serde_json::{Value, json};

/// The chain time, past the start, until which every `keep` carrying a total
/// is mined and reverts.
const REVERTING_UNTIL: u64 = 800;

/// A node in front of the scripted one whose receipt for a `keep` carrying a
/// total, sent before [`REVERTING_UNTIL`], says it reverted.
#[derive(Clone)]
struct Reverting {
    behind: String,
    node: Shared,
    start: u64,
    hashes: Arc<Mutex<HashSet<String>>>,
}

async fn one(reverting: &Reverting, asked: &Value) -> Value {
    let answer: Value = reqwest::Client::new()
        .post(&reverting.behind)
        .json(asked)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let method = asked["method"].as_str().unwrap_or_default();
    if method == "eth_sendTransaction" {
        let tx = &asked["params"][0];
        let data = tx
            .get("input")
            .or_else(|| tx.get("data"))
            .and_then(Value::as_str)
            .unwrap_or("0x");
        let data = alloy::hex::decode(data).unwrap();
        let is_bill = data.len() >= 4
            && data[..4] == IVault::keepCall::SELECTOR
            && IVault::keepCall::abi_decode(&data).is_ok_and(|keep| !keep.total.is_zero());
        let now = reverting.node.lock().unwrap().timestamp;
        if is_bill
            && now < reverting.start + REVERTING_UNTIL
            && let Some(hash) = answer["result"].as_str()
        {
            reverting.hashes.lock().unwrap().insert(hash.to_string());
        }
    }
    if method == "eth_getTransactionReceipt"
        && let Some(hash) = asked["params"][0].as_str()
        && reverting.hashes.lock().unwrap().contains(hash)
    {
        let mut answer = answer;
        answer["result"]["status"] = json!("0x0");
        return answer;
    }
    answer
}

async fn in_front(behind: String, node: Shared, start: u64) -> String {
    let reverting = Reverting {
        behind,
        node,
        start,
        hashes: Default::default(),
    };
    let app = axum::Router::new()
        .route(
            "/",
            post(
                |State(reverting): State<Reverting>, Json(body): Json<Value>| async move {
                    Json(match &body {
                        Value::Array(batch) => {
                            let mut answers = Vec::new();
                            for asked in batch {
                                answers.push(one(&reverting, asked).await);
                            }
                            Value::Array(answers)
                        }
                        asked => one(&reverting, asked).await,
                    })
                },
            ),
        )
        .with_state(reverting);
    serve(app).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_closed_vault_s_last_total_is_sent_before_the_vault_goes_quiet() {
    let start = 1_700_000_005u64;
    // Halted a day ago: its grace is long over, and the window runs from
    // when the keeper first sees it closed.
    let halted_at = start - DAY;
    let node = node(start);
    one_vault(&node);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(
        &node,
        VAULT,
        IVault::tankCall {},
        Tank {
            balance: U256::from(50_000_000u64),
            keepAt: U256::ZERO,
            usageSettled: U256::ZERO,
            usageAsOf: 0,
            lastCollection: halted_at,
            escapeRequestedAt: 0,
            capCarried: U256::ZERO,
            fillPending: false,
        },
    );
    answers(&node, VAULT, IVault::stateCall {}, VaultState::Closed);
    answers(&node, VAULT, IStationVault::haltedAtCall {}, halted_at);
    answers(&node, VAULT, IStationVault::haltGraceCall {}, 600);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(&node, STATION, IStation::stoppedCall {}, true);
    answers(
        &node,
        STATION,
        IStation::valueCall {},
        IStation::valueReturn {
            mark: U256::ZERO,
            exit: U256::ZERO,
        },
    );
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("rounds", 0);
        locked.vars.insert("looked_at", 0);
        // Each round moves the chain on 30 seconds.
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 30;
                *node.vars.get_mut("rounds").unwrap() += 1;
                return Some(Ok(IVault::stateCall::abi_encode_returns(
                    &VaultState::Closed,
                )));
            }
            // The last round in which the vault was still looked at.
            if to == VAULT && data[..4] == IVault::tankCall::SELECTOR {
                let round = node.vars["rounds"];
                node.vars.insert("looked_at", round);
            }
            None
        }));
    }
    // The first total; from REVERTING_UNTIL on, a newer one with the gas of
    // the agent's last orders.
    let clock = node.clone();
    let executor = serve(axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || {
            let clock = clock.clone();
            async move {
                let now = clock.lock().unwrap().timestamp;
                let (total, as_of) = if now >= start + REVERTING_UNTIL {
                    ("1200000", start + REVERTING_UNTIL - 5)
                } else {
                    ("1000000", start - 100)
                };
                axum::Json(json!({
                    "total": total,
                    "as_of": as_of,
                    "signature": "0x01",
                }))
            }
        }),
    ))
    .await;
    let behind = serve_node(node.clone()).await;
    let rpc = in_front(behind, node.clone(), start).await;
    let _keeper = start_keeper_holding("closed-newer-total", &rpc, &executor, 0);

    let newer_sent = || {
        node.lock()
            .unwrap()
            .sent
            .iter()
            .filter(|(to, data)| *to == VAULT && data.len() >= 4)
            .filter_map(|(_, data)| IVault::keepCall::abi_decode(data).ok())
            .any(|keep| keep.total == U256::from(1_200_000u64))
    };
    let reverted_keeps = || {
        node.lock()
            .unwrap()
            .sent
            .iter()
            .filter(|(to, data)| *to == VAULT && data.len() >= 4)
            .filter_map(|(_, data)| IVault::keepCall::abi_decode(data).ok())
            .filter(|keep| keep.total == U256::from(1_000_000u64))
            .count()
    };
    // Well past the window's end (fifteen minutes from the first round), and
    // far short of a chain-day.
    let done = wait_until(Duration::from_secs(240), || {
        newer_sent() || node.lock().unwrap().vars["rounds"] >= 60
    })
    .await;
    let rounds = node.lock().unwrap().vars["rounds"];
    let looked_at = node.lock().unwrap().vars["looked_at"];
    assert!(
        reverted_keeps() >= 3,
        "setup: the first total's keeps were not tried"
    );
    assert!(
        done && newer_sent(),
        "the closed vault's newer total was never sent in {rounds} rounds ({} s of chain time, \
         the window over at about 930 s): the wait left over from the older total's reverted \
         keeps was counted as the vault refusing it, and the vault went quiet for a day ({} \
         keeps carrying the older total were sent and reverted; the vault was last looked at in round \
         {looked_at}, at {} s)",
        rounds * 30,
        reverted_keeps(),
        looked_at * 30
    );
}
