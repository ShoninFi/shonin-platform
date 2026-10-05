//! A halted vault's hold ends with one last try of the latest total, and
//! the bill is written off only if that try reaches the vault and the vault
//! refuses it. A try that never went out (the node turned the `keep` away,
//! or the tank couldn't be read so no `keep` was even built) is not a try:
//! the hold runs on and the next round makes the last try again. The author
//! of a closed vault may take the tank at once, so a newer total sent after
//! the close would race the author.
//!
//! The setup is that of
//! `a_newer_total_held_back_by_reverted_keeps_is_tried_before_the_close_goes`:
//! inside a long grace the halt's total is sent in `keep`s that are mined
//! and revert, so the vault's wait between them reaches about an hour.
//! After the grace the executor answers with a newer total, the cause of the
//! reverts is gone, and `close` would go through. The newer total waits out
//! the vault's wait, so its first try is the hold's last try, fifteen
//! minutes (of chain time; the keeper's own clock is let off) later. That
//! one round, and only it, has a node hiccup:
//!
//! - `the_node_turning_the_last_try_away`: the node turns the first `keep`
//!   carrying the newer total away (a rate limit, not a revert);
//! - `the_tank_unread_at_the_last_try`: the hold's own read of the tank
//!   fails (`header not found`), the round's earlier read having worked.
//!
//! Either way the newer total reaches the vault before `close`.

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

const NEWER: u64 = 1_300_000;

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
    locked.vars.insert("turned_away", 0);
}

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

#[derive(Clone, Copy, PartialEq)]
enum Hiccup {
    /// The node turns the first `keep` carrying the newer total away.
    KeepTurnedAway,
    /// The hold's own read of the tank fails once the hold is over.
    TankUnread,
}

async fn run(test: &str, hiccup: Hiccup) {
    let start = 1_700_000_005u64;
    let halted_at = start;
    let grace = 6_700;
    let grace_end = halted_at + grace;
    let node = node(start);
    a_halted_vault(&node, halted_at, grace);
    let after_grace: Arc<Mutex<Option<Instant>>> = Default::default();
    let reverting = Arc::new(AtomicU64::new(1));
    let reverted = Arc::new(AtomicU64::new(0));
    // Tank reads in the round going on, whether the round's second one (the
    // hold's own) fails, and whether one has.
    let tank_reads = Arc::new(AtomicU64::new(0));
    let tank_fails = Arc::new(AtomicU64::new(0));
    let tank_failed = Arc::new(AtomicU64::new(0));
    {
        let after_grace = after_grace.clone();
        let reverting = reverting.clone();
        let tank_reads_reset = tank_reads.clone();
        let tank_fails_set = tank_fails.clone();
        let tank_failed_once = tank_failed.clone();
        let mut locked = node.lock().unwrap();
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                let mut after = after_grace.lock().unwrap();
                match *after {
                    None if node.timestamp + 200 >= grace_end => {
                        node.timestamp = grace_end;
                        *after = Some(Instant::now());
                        reverting.store(0, Ordering::SeqCst);
                        node.vars.insert("close_possible", 1);
                    }
                    None => node.timestamp += 200,
                    Some(_) => node.timestamp += 30,
                }
                tank_reads_reset.store(0, Ordering::SeqCst);
                // From when the fifteen-minute hold, begun at the grace's
                // end at the earliest, can be over.
                let hold_over = node.timestamp >= grace_end + 900;
                tank_fails_set.store(
                    u64::from(
                        hiccup == Hiccup::TankUnread
                            && hold_over
                            && node.vars["closed"] == 0
                            && tank_failed_once.load(Ordering::SeqCst) == 0,
                    ),
                    Ordering::SeqCst,
                );
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
        locked.on_send = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::closeCall::SELECTOR {
                node.vars.insert("closed", 1);
            }
            if hiccup == Hiccup::KeepTurnedAway
                && to == VAULT
                && node.vars["turned_away"] == 0
                && IVault::keepCall::abi_decode(data)
                    .is_ok_and(|keep| keep.total == U256::from(NEWER))
            {
                node.vars.insert("turned_away", 1);
                return Some(json!({"code": -32005, "message": "rate limited"}));
            }
            None
        }));
        let tank_reads = tank_reads.clone();
        let tank_fails = tank_fails.clone();
        let tank_failed = tank_failed.clone();
        locked.on_request = Some(Arc::new(move |method: &str, params: &Value| {
            if method != "eth_call" {
                return None;
            }
            let tx = &params[0];
            let input = tx
                .get("input")
                .or_else(|| tx.get("data"))
                .and_then(Value::as_str)
                .unwrap_or("0x");
            let data = alloy::hex::decode(input).unwrap_or_default();
            if data.len() < 4 || data[..4] != IVault::tankCall::SELECTOR {
                return None;
            }
            let nth = tank_reads.fetch_add(1, Ordering::SeqCst) + 1;
            let fails = nth == 2 && tank_fails.load(Ordering::SeqCst) == 1;
            if fails {
                tank_failed.store(1, Ordering::SeqCst);
            }
            fails.then(|| json!({"code": -32000, "message": "header not found"}))
        }));
    }
    let total = Arc::new(AtomicU64::new(1_000_000));
    let as_of = Arc::new(AtomicU64::new(start + 1));
    let executor = serve(executor_answering(total.clone(), as_of.clone())).await;
    let behind = serve_node(node.clone()).await;
    let rpc = in_front(behind, reverting.clone(), reverted.clone()).await;
    let _keeper = start_keeper_holding(test, &rpc, &executor, 0);

    assert!(
        wait_until(Duration::from_secs(180), || after_grace
            .lock()
            .unwrap()
            .is_some())
        .await,
        "setup: the grace never ended"
    );
    total.store(NEWER, Ordering::SeqCst);
    as_of.store(grace_end, Ordering::SeqCst);
    let reverted_in_grace = reverted.load(Ordering::SeqCst);
    assert!(
        reverted_in_grace >= 6,
        "setup: the halt's total was not sent and reverted again and again: {reverted_in_grace}"
    );

    let closed = wait_until(Duration::from_secs(300), || {
        node.lock().unwrap().vars["closed"] == 1
    })
    .await;
    // A little longer, to see what follows the close.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let order = order(&node);
    assert!(closed, "setup: the vault never closed: {order:?}");
    if hiccup == Hiccup::KeepTurnedAway {
        assert_eq!(
            node.lock().unwrap().vars["turned_away"],
            1,
            "setup: the newer total's keep was never turned away"
        );
    } else {
        assert_eq!(
            tank_failed.load(Ordering::SeqCst),
            1,
            "setup: the hold's read of the tank never failed"
        );
    }
    let close = order.iter().position(|sent| sent == "close");
    let newer = order
        .iter()
        .position(|sent| *sent == format!("keep {NEWER}"));
    assert!(
        newer.is_some() && newer < close,
        "the hold's last try never reached the vault ({}), yet the newer total was written off \
         and the vault closed before any keep carrying it went through; mined at the vault, in \
         order: {order:?}",
        match hiccup {
            Hiccup::KeepTurnedAway => "the node turned its keep away",
            Hiccup::TankUnread => "the hold's read of the tank failed",
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn the_node_turning_the_last_try_away() {
    run("last-try-turned-away", Hiccup::KeepTurnedAway).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn the_tank_unread_at_the_last_try() {
    run("last-try-tank-unread", Hiccup::TankUnread).await;
}
