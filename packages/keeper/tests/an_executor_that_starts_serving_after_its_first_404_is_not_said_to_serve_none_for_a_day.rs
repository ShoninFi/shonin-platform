//! A fresh start: the keeper and the executor come up together (a deploy, or
//! both restarted), or a vault opens before the executor has taken it on.
//! The keeper's first round asks for the open vault's total at once; the
//! executor has not signed one yet and answers 404. A little later it serves
//! the vault's total, as it should.
//!
//! An open vault is asked once a chain-day, but a 404 for a vault the keeper
//! still wants a total for is asked again on the same wait as an ask that
//! got no answer (a chain-minute, doubling to an hour), until it gets a
//! total. So the vault's total is collected well inside the day, and
//! "executor serves no vault" doesn't stand, a 503, for the rest of it.

mod process;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use axum::response::IntoResponse;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    STATION, VAULT, a_spec, answers, node, one_vault, serve, serve_node,
    start_keeper_serving_none_for, timers,
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_executor_that_starts_serving_after_its_first_404_is_not_said_to_serve_none_for_a_day() {
    let start = 1_700_000_000u64;
    let node = node(start);
    one_vault(&node);
    answers(&node, VAULT, IVault::stateCall {}, VaultState::Open);
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
            lastCollection: start - 3_600,
            escapeRequestedAt: 0,
            capCarried: U256::ZERO,
            fillPending: false,
        },
    );
    answers(&node, VAULT, IStationVault::haltedAtCall {}, 0);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(&node, STATION, IStation::outsideBandCall {}, false);

    // The executor has not taken the vault on yet: 404, until it has.
    let serving = Arc::new(AtomicBool::new(false));
    let asked = Arc::new(AtomicU64::new(0));
    let executor = {
        let serving = serving.clone();
        let asked = asked.clone();
        serve(axum::Router::new().route(
            "/usage/{chain}/{vault}",
            axum::routing::get(move || {
                let serving = serving.clone();
                let asked = asked.clone();
                async move {
                    asked.fetch_add(1, Ordering::SeqCst);
                    if serving.load(Ordering::SeqCst) {
                        axum::Json(json!({
                            "total": "1000000",
                            "as_of": start - 1,
                            "signature": "0x01",
                        }))
                        .into_response()
                    } else {
                        axum::http::StatusCode::NOT_FOUND.into_response()
                    }
                }
            }),
        ))
        .await
    };
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper_serving_none_for("executor-404-then-served", &rpc, &executor, 2);

    // The first ask is answered 404, and said.
    let mut said = false;
    for _ in 0..300 {
        if keeper.logged().contains("serves no total") {
            said = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        said,
        "setup: the first ask was not answered 404 (asked {}): {}",
        asked.load(Ordering::SeqCst),
        keeper.logged()
    );
    // The executor takes the vault on and serves its total from now on, while
    // chain time moves on two minutes at a time, for well under two hours
    // (well inside the chain-day), over well past the shortened hour.
    serving.store(true, Ordering::SeqCst);
    for _ in 0..24 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        node.lock().unwrap().timestamp += 120;
    }
    let health = keeper.health().await;
    let asks = asked.load(Ordering::SeqCst);
    assert!(
        health
            .as_ref()
            .is_some_and(|(_, said)| !said.contains("executor serves no vault")),
        "the executor serves the vault's total (it was asked {asks} times in all), yet the \
         keeper's health still says it serves no vault: {health:?}"
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
    assert_eq!(
        keeps, 1,
        "the vault's bill was not collected once the executor served its total (asked {asks} \
         times in all)"
    );
}
