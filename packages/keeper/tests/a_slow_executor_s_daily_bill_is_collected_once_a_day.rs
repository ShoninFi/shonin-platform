//! One open vault whose agent runs, so the executor's signed total grows.
//! The executor answers every ask, always, in two and a half seconds. Each
//! round is a minute of chain time on.
//!
//! The executor is asked once a day for an open vault's total, and the
//! answer, whenever it comes back, is collected once, in a round after. So
//! in fourteen minutes one `keep` with a total goes out, not one a minute,
//! and the executor is asked once.

mod process;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    STATION, VAULT, a_spec, a_tank, answers, node, one_vault, serve, serve_node, start_keeper,
    timers, wait_until,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_slow_executor_s_daily_bill_is_collected_once_a_day() {
    let node = node(1_700_000_005);
    one_vault(&node);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(&node, VAULT, IVault::tankCall {}, a_tank(5_000_000));
    answers(&node, VAULT, IStationVault::haltedAtCall {}, 0);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(&node, STATION, IStation::outsideBandCall {}, false);
    node.lock().unwrap().vars.insert("rounds", 0);
    // A minute of chain time a round.
    node.lock().unwrap().on_call = Some(Arc::new(move |node, to, data| {
        if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
            node.timestamp += 61;
            *node.vars.get_mut("rounds").unwrap() += 1;
            return Some(Ok(IVault::stateCall::abi_encode_returns(&VaultState::Open)));
        }
        None
    }));
    // The agent runs: each answer is a larger total, signed later.
    let asks = Arc::new(AtomicU64::new(0));
    let counted = asks.clone();
    let executor = serve(axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || {
            let asks = counted.clone();
            async move {
                let n = asks.fetch_add(1, Ordering::SeqCst) + 1;
                tokio::time::sleep(Duration::from_millis(2_500)).await;
                axum::Json(serde_json::json!({
                    "total": (1_000_000 + n * 1_000).to_string(),
                    "as_of": 1_700_000_001u64 + n * 60,
                    "signature": "0x01",
                }))
            }
        }),
    ))
    .await;
    let rpc = serve_node(node.clone()).await;
    let _keeper = start_keeper("slow-executor-daily", &rpc, &executor);

    assert!(
        wait_until(Duration::from_secs(150), || {
            node.lock().unwrap().vars["rounds"] >= 14
        })
        .await,
        "the keeper did not run fourteen rounds"
    );
    let rounds = node.lock().unwrap().vars["rounds"];
    let keeps = node
        .lock()
        .unwrap()
        .sent
        .iter()
        .filter(|(to, data)| {
            *to == VAULT
                && data.len() >= 4
                && data[..4] == IVault::keepCall::SELECTOR
                && IVault::keepCall::abi_decode(data).is_ok_and(|keep| !keep.total.is_zero())
        })
        .count();
    let asked = asks.load(Ordering::SeqCst);
    assert_eq!(
        (keeps, asked),
        (1, 1),
        "{keeps} keeps with a total and {asked} asks in {rounds} rounds (a minute of chain \
         time each) for one open vault whose executor answers every ask in 2.5 s: the daily \
         bill is to be asked for and collected once"
    );
}
