//! A halted vault's close is held for its bill for at most fifteen minutes,
//! in chain time and on the keeper's clock alike, from the start of the wait
//! going on now. The keeper's own clock is let off here, and the chain's
//! jumps ahead.
//!
//! The close would go through, its bill's first `keep` is turned away by the
//! node (a rate limit), and the hold starts. Then the close stops being
//! possible (a stranger's gift and valuation, say) and the bill goes through
//! in the next round, which ends that wait. Sixteen minutes later the close
//! would go through again and the executor has a newer total, whose first
//! `keep` the node turns away once more. That close has been waiting for its
//! bill for one round, not sixteen minutes: it waits for the bill, which
//! goes through a round later, before the close goes, since the author of a
//! closed vault may take the tank at once.

mod process;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    STATION, Shared, VAULT, WAD, a_spec, answers, node, one_vault, serve, serve_node,
    start_keeper_holding, timers, wait_until,
};
use serde_json::json;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_halted_vault_s_close_hold_is_measured_from_the_wait_going_on_now() {
    let start = 1_700_000_005u64;
    // A long grace: the whole test runs inside it.
    let halted_at = start - 60;
    let node = node(start);
    a_halted_vault(&node, halted_at, 3 * 3_600);
    let began = Instant::now();
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("close_possible", 1);
        locked.vars.insert("turn_away", 1_000_000);
        locked.vars.insert("turned_away", 0);
        locked.vars.insert("ahead", 0);
        // The chain's clock runs with the wall clock, and as far ahead of it
        // as `ahead`.
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp = start + began.elapsed().as_secs() + node.vars["ahead"];
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
        // The first `keep` carrying the total named in `turn_away` is turned
        // away by the node, as a gateway's rate limit does.
        locked.on_send = Some(Arc::new(|node, to, data| {
            if to != VAULT {
                return None;
            }
            if let Ok(keep) = IVault::keepCall::abi_decode(data)
                && keep.total == U256::from(node.vars["turn_away"])
            {
                node.vars.insert("turn_away", 0);
                *node.vars.get_mut("turned_away").unwrap() += 1;
                // The first time: a stranger's gift and valuation follow, and
                // the close is no longer possible for now.
                if node.vars["turned_away"] == 1 {
                    node.vars.insert("close_possible", 0);
                }
                return Some(json!({"code": -32005, "message": "rate limited"}));
            }
            if data[..4] == IVault::closeCall::SELECTOR {
                node.vars.insert("closed", 1);
            }
            None
        }));
    }
    let total = Arc::new(AtomicU64::new(1_000_000));
    let as_of = Arc::new(AtomicU64::new(start - 1));
    let executor = serve(executor_answering(total.clone(), as_of.clone())).await;
    let rpc = serve_node(node.clone()).await;
    let _keeper = start_keeper_holding("hold-of-this-wait", &rpc, &executor, 0);

    // The close is held for the first bill, which then goes through.
    assert!(
        wait_until(Duration::from_secs(60), || {
            order(&node).iter().any(|sent| sent == "keep 1000000")
        })
        .await,
        "setup: the first bill was never collected: {:?}",
        order(&node)
    );
    assert_eq!(
        node.lock().unwrap().vars["turned_away"],
        1,
        "setup: the first bill's keep was not turned away once"
    );
    assert_eq!(
        node.lock().unwrap().vars["closed"],
        0,
        "setup: closed early"
    );

    // Sixteen minutes on: the close would go through again, the executor
    // has a newer total, and its first keep is turned away.
    let ahead = 16 * 60;
    total.store(1_300_000, Ordering::SeqCst);
    as_of.store(
        start + began.elapsed().as_secs() + ahead - 2,
        Ordering::SeqCst,
    );
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("ahead", ahead);
        locked.vars.insert("turn_away", 1_300_000);
        locked.vars.insert("close_possible", 1);
    }
    assert!(
        wait_until(
            Duration::from_secs(60),
            || node.lock().unwrap().vars["closed"] == 1
        )
        .await,
        "setup: the vault never closed: {:?}",
        order(&node)
    );
    let order = order(&node);
    let close = order.iter().position(|sent| sent == "close");
    let newer = order.iter().position(|sent| sent == "keep 1300000");
    assert!(
        newer.is_some() && newer < close,
        "the close was let go with the newer bill uncollected: one keep carrying it was turned \
         away by the node in the round the close first became possible again, and the close \
         went out in that same round, its fifteen minutes counted from an earlier wait whose \
         bill had gone through; mined at the vault, in order: {order:?}"
    );
}
