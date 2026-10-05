//! One open vault with a tank and a bill. The executor fails the first ask
//! (a 503, as from one restarting behind a proxy) and answers every ask
//! after it. Each round is a minute of chain time.
//!
//! An ask that got no answer is made again a chain-minute on (then two,
//! four, up to an hour) until it is answered, and the executor is said not
//! to answer only after fifteen minutes of no answers. So a five-second blip
//! neither holds `/health` down nor puts the day's bill off a day: fifteen
//! minutes on, the bill has been collected once and the keeper is up.

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
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_executor_that_missed_one_ask_is_not_said_down_for_a_day() {
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
    node.lock().unwrap().on_call = Some(Arc::new(move |node, to, data| {
        if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
            node.timestamp += 61;
            *node.vars.get_mut("rounds").unwrap() += 1;
            return Some(Ok(IVault::stateCall::abi_encode_returns(&VaultState::Open)));
        }
        None
    }));
    // The first ask fails; every one after it is answered at once.
    let asks = Arc::new(AtomicU64::new(0));
    let counted = asks.clone();
    let executor = serve(axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || {
            let asks = counted.clone();
            async move {
                if asks.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(axum::http::StatusCode::SERVICE_UNAVAILABLE);
                }
                Ok(axum::Json(json!({
                    "total": "1000000",
                    "as_of": 1_700_000_001u64,
                    "signature": "0x01",
                })))
            }
        }),
    ))
    .await;
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("executor-blip", &rpc, &executor);

    assert!(
        wait_until(Duration::from_secs(120), || {
            node.lock().unwrap().vars["rounds"] >= 15
        })
        .await,
        "the keeper did not run fifteen rounds"
    );
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
    let health = keeper.health_until(Duration::from_secs(5), |_| true).await;
    assert!(
        health.as_ref().is_some_and(|(status, _)| *status == 200) && keeps == 1,
        "fifteen minutes after one failed ask, the executor answering every ask since: {asked} \
         asks, {keeps} keeps with a total, health {health:?}"
    );
}
