//! One open vault with a tank and a bill to collect, on a chain whose clock
//! runs with the wall clock. The executor answers every ask, always, in
//! three seconds, well inside its five-second timeout. The keeper's rounds
//! take longer than that (here about 27 seconds; a keeper with many vaults
//! takes as long).
//!
//! The round doesn't wait for the answer; the answer that comes back is
//! kept, and the bill is collected with it in a round after.

mod process;

use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    STATION, VAULT, a_spec, a_tank, answers, node, one_vault, serve, serve_node, start_keeper,
    timers, wait_until,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_executor_that_answers_slowly_still_has_its_bill_collected() {
    let origin = 1_700_000_005u64;
    let started = Instant::now();
    let node = node(origin);
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
    // The chain's clock is the wall clock; each round starts with a read
    // that takes 25 seconds, standing in for a round with many vaults.
    node.lock().unwrap().on_call = Some(Arc::new(move |node, to, data| {
        if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
            std::thread::sleep(Duration::from_secs(25));
            node.timestamp = origin + started.elapsed().as_secs();
            return Some(Ok(IVault::stateCall::abi_encode_returns(&VaultState::Open)));
        }
        node.timestamp = origin + started.elapsed().as_secs();
        None
    }));
    // The executor runs on a runtime of its own: the node's slow read holds
    // this test's threads, and the executor is a service apart from it.
    let (url_sent, url) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let executor = serve(axum::Router::new().route(
                    "/usage/{chain}/{vault}",
                    axum::routing::get(|| async {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        axum::Json(serde_json::json!({
                            "total": "1000000",
                            "as_of": 1_700_000_001u64,
                            "signature": "0x01",
                        }))
                    }),
                ))
                .await;
                url_sent.send(executor).unwrap();
                std::future::pending::<()>().await;
            });
    });
    let executor = url.recv().unwrap();
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("slow-executor", &rpc, &executor);

    let collected = wait_until(Duration::from_secs(230), || {
        node.lock().unwrap().sent.iter().any(|(to, data)| {
            *to == VAULT
                && data.len() >= 4
                && data[..4] == IVault::keepCall::SELECTOR
                && IVault::keepCall::abi_decode(data).is_ok_and(|keep| !keep.total.is_zero())
        })
    })
    .await;
    let mut keeper = keeper;
    let exited = keeper.exited();
    let health = keeper.health_until(Duration::from_secs(30), |_| true).await;
    let logged = keeper.logged();
    let asked = logged
        .lines()
        .filter(|line| line.contains("could not be asked"))
        .count();
    let stateful = node.lock().unwrap().calls.len();
    assert!(
        collected,
        "in {:?} the bill of an executor that answers every ask in three seconds was never \
         collected; keeper exited: {exited:?}; health {health:?}; 'could not be asked' \
         warnings: {asked}; calls made: {stateful}",
        started.elapsed()
    );
}
