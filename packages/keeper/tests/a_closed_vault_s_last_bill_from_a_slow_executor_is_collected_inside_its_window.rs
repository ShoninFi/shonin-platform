//! A closed vault inside its last bill's window, its station stopped and
//! empty. The executor answers every ask, always, in two and a half seconds,
//! with a newer total each time: the agent's last orders landing. Each round
//! is a minute of chain time on.
//!
//! In the window the executor is asked once a minute, and each newer total
//! that has come back is collected in a round after: the bill is collected
//! inside the window, and no total goes out in more than one `keep`.

mod process;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    STATION, VAULT, a_spec, answers, node, one_vault, serve, serve_node, start_keeper, timers,
    wait_until,
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_closed_vault_s_last_bill_from_a_slow_executor_is_collected_inside_its_window() {
    let start = 1_700_000_000u64;
    // Halted with a ten-minute grace; the window runs fifteen minutes past
    // its end.
    let halted_at = start - 100;
    let window_end = halted_at + 600 + 15 * 60;
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
        locked.vars.insert("first_keep_at", 0);
        locked.on_call = Some(Arc::new(|node, to, data| {
            // A chain-minute a round.
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 61;
                *node.vars.get_mut("rounds").unwrap() += 1;
                return Some(Ok(IVault::stateCall::abi_encode_returns(
                    &VaultState::Closed,
                )));
            }
            None
        }));
        locked.on_send = Some(Arc::new(|node, to, data| {
            if to == VAULT
                && data[..4] == IVault::keepCall::SELECTOR
                && node.vars["first_keep_at"] == 0
            {
                let now = node.timestamp;
                node.vars.insert("first_keep_at", now);
            }
            None
        }));
    }
    // The executor signs a new total, as of the chain's head, each time,
    // two and a half seconds after it is asked.
    let asked_of = node.clone();
    let asks = Arc::new(AtomicU64::new(0));
    let counted = asks.clone();
    let executor = serve(axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || {
            let node = asked_of.clone();
            let asks = counted.clone();
            async move {
                asks.fetch_add(1, Ordering::SeqCst);
                let now = node.lock().unwrap().timestamp;
                tokio::time::sleep(Duration::from_millis(2_500)).await;
                axum::Json(json!({
                    "total": (now - 1_699_000_000).to_string(),
                    "as_of": now,
                    "signature": format!("0x{}", "11".repeat(65)),
                }))
            }
        }),
    ))
    .await;
    let rpc = serve_node(node.clone()).await;
    let _keeper = start_keeper("closed-slow-executor", &rpc, &executor);

    assert!(
        wait_until(Duration::from_secs(150), || {
            node.lock().unwrap().vars["rounds"] >= 12
        })
        .await,
        "the keeper did not run twelve rounds"
    );
    let (first_keep_at, keeps): (u64, Vec<u64>) = {
        let locked = node.lock().unwrap();
        let keeps = locked
            .sent
            .iter()
            .filter(|(to, data)| *to == VAULT && data.len() >= 4)
            .filter_map(|(_, data)| IVault::keepCall::abi_decode(data).ok())
            .map(|keep| keep.asOf)
            .collect();
        (locked.vars["first_keep_at"], keeps)
    };
    let asked = asks.load(Ordering::SeqCst);
    assert!(
        first_keep_at != 0 && first_keep_at < window_end,
        "the closed vault's last bill was not collected inside its window (first keep at \
         {first_keep_at}, the window ends at {window_end}); {asked} asks"
    );
    let mut distinct = keeps.clone();
    distinct.dedup();
    assert_eq!(
        distinct, keeps,
        "a total went out in more than one keep: {keeps:?}"
    );
    assert!(
        keeps.len() as u64 <= asked,
        "{} keeps for {asked} answers",
        keeps.len()
    );
}
