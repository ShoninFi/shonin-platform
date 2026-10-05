//! A halted vault inside its grace whose `close` would go through. The close
//! goes only once the `keep` for the latest answer went through, or after at
//! most fifteen minutes from when it was first held for its bill; the hold
//! ends when the bill settles.
//!
//! The node turns away every `keep` carrying the first total (a gateway's
//! rate limit), so the close waits for it. Sixteen minutes on (the chain's
//! clock jumps; the keeper's own hold is let off) the node takes it: the
//! round's first step collects it, the bill is settled, and the hold is
//! over. The same round asks the executor again before the close, and the
//! executor has signed a newer total since; the node turns its first `keep`
//! away. The close is held for it afresh, a round old: the newer total is
//! collected before the close goes, since the author of a closed vault may
//! take the tank at once.

mod process;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    STATION, VAULT, a_spec, answers, node, one_vault, serve, serve_node, start_keeper_holding,
    timers, wait_until,
};
use serde_json::json;

const FIRST: u64 = 1_000_000;
const NEWER: u64 = 1_300_000;

fn order(node: &process::Shared) -> Vec<String> {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_newer_total_after_a_settled_bill_gets_its_own_hold() {
    let start = 1_700_000_005u64;
    let halted_at = start - 60;
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
            lastCollection: halted_at - 3_600,
            escapeRequestedAt: 0,
            capCarried: U256::ZERO,
            fillPending: false,
        },
    );
    answers(&node, VAULT, IStationVault::haltedAtCall {}, halted_at);
    // A long grace: the whole test runs inside it.
    answers(&node, VAULT, IStationVault::haltGraceCall {}, 3 * 3_600u64);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(&node, STATION, IStation::stoppedCall {}, false);
    answers(&node, STATION, IStation::outsideBandCall {}, false);

    let began = Instant::now();
    // The first total is collected once this is set; the executor signs the
    // newer one from then on.
    let first_collected = Arc::new(AtomicBool::new(false));
    let rounds = Arc::new(AtomicU64::new(0));
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("closed", 0);
        locked.vars.insert("ahead", 0);
        // Set by the test; taken up at the next round's start, so the
        // round's first step is the first to find the node taking the bill.
        locked.vars.insert("armed", 0);
        locked.vars.insert("turn_away_first", 1);
        locked.vars.insert("newer_turned_away", 0);
        let counted = rounds.clone();
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                counted.fetch_add(1, Ordering::SeqCst);
                if node.vars["armed"] == 1 {
                    node.vars.insert("armed", 2);
                    node.vars.insert("ahead", 16 * 60);
                    node.vars.insert("turn_away_first", 0);
                }
                node.timestamp = start + began.elapsed().as_secs() + node.vars["ahead"];
                let state = if node.vars["closed"] == 1 {
                    VaultState::Closed
                } else {
                    VaultState::Killed
                };
                return Some(Ok(IVault::stateCall::abi_encode_returns(&state)));
            }
            if to == VAULT && data[..4] == IVault::closeCall::SELECTOR {
                return Some(if node.vars["closed"] == 0 {
                    Ok(Vec::new())
                } else {
                    Err("Closed()".to_string())
                });
            }
            None
        }));
        let collected = first_collected.clone();
        locked.on_send = Some(Arc::new(move |node, to, data| {
            if to != VAULT {
                return None;
            }
            if let Ok(keep) = IVault::keepCall::abi_decode(data) {
                if keep.total == U256::from(FIRST) {
                    if node.vars["turn_away_first"] == 1 {
                        return Some(json!({"code": -32005, "message": "rate limited"}));
                    }
                    collected.store(true, Ordering::SeqCst);
                }
                // The newer total's first keep is turned away too.
                if keep.total == U256::from(NEWER) && node.vars["newer_turned_away"] == 0 {
                    node.vars.insert("newer_turned_away", 1);
                    return Some(json!({"code": -32005, "message": "rate limited"}));
                }
            }
            if data[..4] == IVault::closeCall::SELECTOR {
                node.vars.insert("closed", 1);
            }
            None
        }));
    }
    let signed_newer = first_collected.clone();
    let executor = serve(axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || {
            let newer = signed_newer.load(Ordering::SeqCst);
            async move {
                let (total, as_of) = if newer {
                    (NEWER, start + 100)
                } else {
                    (FIRST, start - 1)
                };
                axum::Json(json!({
                    "total": total.to_string(),
                    "as_of": as_of,
                    "signature": "0x01",
                }))
            }
        }),
    ))
    .await;
    let rpc = serve_node(node.clone()).await;
    let _keeper = start_keeper_holding("newer-total-own-hold", &rpc, &executor, 0);

    // The close waits for the first bill, turned away round after round.
    assert!(
        wait_until(Duration::from_secs(60), || rounds.load(Ordering::SeqCst)
            >= 4)
        .await,
        "setup: the keeper did not run"
    );
    assert_eq!(
        node.lock().unwrap().vars["closed"],
        0,
        "setup: closed while its bill was turned away: {:?}",
        order(&node)
    );
    node.lock().unwrap().vars.insert("armed", 1);
    assert!(
        wait_until(Duration::from_secs(90), || {
            node.lock().unwrap().vars["closed"] == 1
        })
        .await,
        "setup: the vault never closed: {:?}",
        order(&node)
    );
    let order = order(&node);
    let close = order.iter().position(|sent| sent == "close");
    let newer = order
        .iter()
        .position(|sent| *sent == format!("keep {NEWER}"));
    assert!(
        newer.is_some() && newer < close,
        "the close went with the newer total uncollected: its first keep was turned away in \
         the round the first bill went through, and the newer total was written off at once \
         on the first bill's hold; mined at the vault, in \
         order: {order:?}"
    );
}
