//! A closed vault, its station stopped and empty, nothing waiting. The
//! executor serves a total newer than the vault collected, and the vault
//! refuses every `keep` carrying it (`NotBlessed`: the key's blessing lapsed
//! after the executor let the vault go). The tank still holds money.
//!
//! Once the last bill's window is over the keeper takes the refusal as
//! settling the bill and lets the vault go quiet: it has nothing more to do
//! for it. The vault is then no longer said to refuse its bill, and the
//! keeper's health is up again.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    DAY, STATION, VAULT, a_spec, answers, called, name, node, one_vault, serve, serve_node,
    start_keeper_holding, timers, wait_until,
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_closed_vault_s_settled_refused_bill_does_not_hold_health_down_for_good() {
    let start = 1_700_000_005u64;
    let halted_at = start - 3 * DAY;
    let grace_end = halted_at + 600;
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
            usageSettled: U256::from(1_000_000u64),
            usageAsOf: grace_end - 10,
            lastCollection: grace_end - 10,
            escapeRequestedAt: 0,
            capCarried: U256::from(3_000_000u64),
            fillPending: false,
        },
    );
    answers(&node, VAULT, IStationVault::haltedAtCall {}, halted_at);
    answers(&node, VAULT, IStationVault::haltGraceCall {}, 600);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
    answers(
        &node,
        STATION,
        IStation::specCall {},
        a_spec(vec![name("local")]),
    );
    answers(
        &node,
        STATION,
        IStation::approvedKeyCall {
            driver: name("local"),
        },
        Address::ZERO,
    );
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
        locked.vars.insert("refused", 0);
        // A minute of chain time a round.
        locked.on_call = Some(Arc::new(|node, to, data| {
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
                && IVault::keepCall::abi_decode(data).is_ok_and(|keep| !keep.total.is_zero())
            {
                *node.vars.get_mut("refused").unwrap() += 1;
                return Some(json!({"code": 3, "message": "execution reverted: NotBlessed()"}));
            }
            None
        }));
    }
    let executor = serve(axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || async move {
            axum::Json(json!({
                "total": "1200000",
                "as_of": grace_end + 5,
                "signature": "0x01",
            }))
        }),
    ))
    .await;
    let rpc = serve_node(node.clone()).await;
    // The window is judged on the chain's clock, which runs a minute a round.
    let keeper = start_keeper_holding("closed-refused-settled", &rpc, &executor, 0);

    // The vault goes quiet: its timers are no longer read each round.
    let mut quiet = false;
    for _ in 0..90 {
        let before = called::<IVault::timersCall>(&node, VAULT);
        let rounds = node.lock().unwrap().vars["rounds"];
        wait_until(Duration::from_secs(10), || {
            node.lock().unwrap().vars["rounds"] >= rounds + 3
        })
        .await;
        if called::<IVault::timersCall>(&node, VAULT) == before
            && node.lock().unwrap().vars["refused"] > 0
        {
            quiet = true;
            break;
        }
    }
    assert!(quiet, "the closed vault never went quiet");

    // The keeper has settled the bill and has nothing more to do for the
    // vault, so its health is up again.
    let health = keeper
        .health_until(Duration::from_secs(20), |status| status == 200)
        .await;
    assert!(
        health.as_ref().is_some_and(|(status, _)| *status == 200),
        "the keeper let the closed vault go quiet, its refused bill settled, and still answers \
         its health as down for that bill: {health:?}"
    );
}
