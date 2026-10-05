//! Inside a long grace a halted vault's total is sent in `keep`s that are
//! mined and revert, again and again, so the keeper waits ever longer
//! between them, up to an hour. After the grace the executor answers with a
//! newer total, the cause of the reverts is gone, and the close would go
//! through. The keeper's own clock is let off here.
//!
//! The wait between reverted `keep`s is the vault's, so the newer total
//! waits it out too, and the close is held for it. When the hold is over the
//! latest total gets one last try, whatever the wait: the newer total is
//! sent, and collected, before the close goes, since the author of a closed
//! vault may take the tank at once.

mod process;

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use axum::Json;
use axum::extract::State;
use axum::routing::post;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    STATION, Shared, VAULT, WAD, a_spec, answers, node, one_vault, serve, serve_node,
    start_keeper_holding, timers, wait_until,
};
use serde_json::{Value, json};

/// A halted vault, its station stopped and empty, nothing waiting; whether
/// `close` would go through is the test's `close_possible`, and it closes
/// when a `close` is mined.
fn a_halted_vault(node: &Shared, halted_at: u64, grace: u64) {
    one_vault(node);
    answers(node, VAULT, IVault::timersCall {}, timers());
    answers(
        node,
        VAULT,
        IVault::tankCall {},
        Tank {
            balance: U256::from(50_000_000u64),
            keepAt: U256::ZERO,
            usageSettled: U256::ZERO,
            usageAsOf: 0,
            lastCollection: halted_at - 3_600,
            escapeRequestedAt: 0,
            capCarried: U256::ZERO,
            fillPending: false,
        },
    );
    answers(node, VAULT, IStationVault::haltedAtCall {}, halted_at);
    answers(node, VAULT, IStationVault::haltGraceCall {}, grace);
    answers(node, VAULT, IVault::homeStationCall {}, STATION);
    answers(node, VAULT, IStationVault::unsettledCall {}, false);
    answers(node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    answers(node, VAULT, IVault::requestCountCall {}, U256::ZERO);
    answers(node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(node, STATION, IStation::stoppedCall {}, true);
    answers(node, STATION, IStation::outsideBandCall {}, false);
    answers(
        node,
        STATION,
        IStation::valueCall {},
        IStation::valueReturn {
            mark: U256::ZERO,
            exit: U256::ZERO,
        },
    );
    answers(
        node,
        STATION,
        IStation::unwindCall {
            fraction: U256::from(WAD),
        },
        IStation::unwindReturn {
            sentHome: U256::ZERO,
            blocked: false,
        },
    );
    let mut locked = node.lock().unwrap();
    locked.vars.insert("closed", 0);
    locked.vars.insert("close_possible", 0);
}

/// What was mined at the vault, in order: `keep`s by their total, and
/// `close`.
fn order(node: &Shared) -> Vec<String> {
    node.lock()
        .unwrap()
        .sent
        .iter()
        .filter(|(to, data)| *to == VAULT && data.len() >= 4)
        .filter_map(|(_, data)| {
            if let Ok(keep) = IVault::keepCall::abi_decode(data) {
                Some(format!("keep {}", keep.total))
            } else if data[..4] == IVault::closeCall::SELECTOR {
                Some("close".to_string())
            } else {
                None
            }
        })
        .collect()
}

fn executor_answering(total: Arc<AtomicU64>, as_of: Arc<AtomicU64>) -> axum::Router {
    axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || {
            let (total, as_of) = (total.clone(), as_of.clone());
            async move {
                axum::Json(json!({
                    "total": total.load(Ordering::SeqCst).to_string(),
                    "as_of": as_of.load(Ordering::SeqCst),
                    "signature": "0x01",
                }))
            }
        }),
    )
}

/// A node in front of the scripted one whose receipt for every `keep`
/// carrying a total, sent while `reverting` is set, says it reverted.
#[derive(Clone)]
struct Reverting {
    behind: String,
    reverting: Arc<AtomicU64>,
    hashes: Arc<Mutex<HashSet<String>>>,
    reverted: Arc<AtomicU64>,
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
        if is_bill
            && reverting.reverting.load(Ordering::SeqCst) == 1
            && let Some(hash) = answer["result"].as_str()
        {
            reverting.reverted.fetch_add(1, Ordering::SeqCst);
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

async fn in_front(behind: String, reverting: Arc<AtomicU64>, reverted: Arc<AtomicU64>) -> String {
    let reverting = Reverting {
        behind,
        reverting,
        hashes: Default::default(),
        reverted,
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
async fn a_newer_total_held_back_by_reverted_keeps_is_tried_before_the_close_goes() {
    let start = 1_700_000_005u64;
    let halted_at = start;
    // Long enough for the waits between reverted keeps to reach most of an
    // hour: the last one inside it is sent at 6,600 seconds and the next
    // would be an hour later.
    let grace = 6_700;
    let grace_end = halted_at + grace;
    let node = node(start);
    a_halted_vault(&node, halted_at, grace);
    // Inside the grace the chain runs 200 seconds a round (never landing in a
    // valuation window); from the grace's end, 30 seconds a round.
    let after_grace: Arc<Mutex<Option<Instant>>> = Default::default();
    let reverting = Arc::new(AtomicU64::new(1));
    let reverted = Arc::new(AtomicU64::new(0));
    {
        let after_grace = after_grace.clone();
        let reverting = reverting.clone();
        let mut locked = node.lock().unwrap();
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                let mut after = after_grace.lock().unwrap();
                match *after {
                    None if node.timestamp + 200 >= grace_end => {
                        node.timestamp = grace_end;
                        *after = Some(Instant::now());
                        // The reverts' cause has passed, and a valuation
                        // after the halt has found the station empty.
                        reverting.store(0, Ordering::SeqCst);
                        node.vars.insert("close_possible", 1);
                    }
                    None => node.timestamp += 200,
                    Some(_) => node.timestamp += 30,
                }
                let state = if node.vars["closed"] == 1 {
                    VaultState::Closed
                } else {
                    VaultState::Killed
                };
                return Some(Ok(IVault::stateCall::abi_encode_returns(&state)));
            }
            if to == VAULT && data[..4] == IVault::closeCall::SELECTOR {
                return Some(
                    if node.vars["close_possible"] == 1 && node.vars["closed"] == 0 {
                        Ok(Vec::new())
                    } else {
                        Err("NotYet()".to_string())
                    },
                );
            }
            None
        }));
        locked.vars.insert("newer_at", 0);
        locked.on_send = Some(Arc::new(|node, to, data| {
            if to == VAULT && data[..4] == IVault::closeCall::SELECTOR {
                node.vars.insert("closed", 1);
            }
            if to == VAULT
                && node.vars["newer_at"] == 0
                && IVault::keepCall::abi_decode(data)
                    .is_ok_and(|keep| keep.total == U256::from(1_300_000u64))
            {
                let at = node.timestamp;
                node.vars.insert("newer_at", at);
            }
            None
        }));
    }
    // The halt's total; after the grace, the whole of it.
    let total = Arc::new(AtomicU64::new(1_000_000));
    let as_of = Arc::new(AtomicU64::new(start + 1));
    let executor = serve(executor_answering(total.clone(), as_of.clone())).await;
    let behind = serve_node(node.clone()).await;
    let rpc = in_front(behind, reverting.clone(), reverted.clone()).await;
    let _keeper = start_keeper_holding("never-sent-total", &rpc, &executor, 0);

    assert!(
        wait_until(Duration::from_secs(180), || after_grace
            .lock()
            .unwrap()
            .is_some())
        .await,
        "setup: the grace never ended"
    );
    total.store(1_300_000, Ordering::SeqCst);
    as_of.store(grace_end, Ordering::SeqCst);
    let reverted_in_grace = reverted.load(Ordering::SeqCst);
    assert!(
        reverted_in_grace >= 6,
        "setup: the halt's total was not sent and reverted again and again: {reverted_in_grace}"
    );

    // Fifteen minutes of hold, on the chain's clock, and a margin.
    let closed = wait_until(Duration::from_secs(300), || {
        node.lock().unwrap().vars["closed"] == 1
    })
    .await;
    let order = order(&node);
    assert!(closed, "setup: the vault never closed: {order:?}");
    let close = order.iter().position(|sent| sent == "close");
    let newer = order.iter().position(|sent| sent == "keep 1300000");
    assert!(
        newer.is_some() && newer < close,
        "the vault was closed with the newer total ({}) written off without one keep carrying \
         it ever being sent: the wait between reverted keeps ({} of them) was not set aside for \
         the last try when the hold was over; mined at the vault, in order: {order:?}",
        1_300_000,
        reverted_in_grace
    );
    let newer_at = node.lock().unwrap().vars["newer_at"];
    assert!(
        newer_at >= grace_end + 300,
        "setup: the newer total did not wait out the vault's wait (sent at {newer_at}, the \
         grace over at {grace_end})"
    );
}
