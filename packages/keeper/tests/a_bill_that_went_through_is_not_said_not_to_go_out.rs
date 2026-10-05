//! A halted vault past its grace that `close()` would close. In one round
//! the keeper may send the same `keep` (the same total, the same bytes)
//! twice: once as the round's bill ("keep") and once before the close
//! ("collect the bill before the vault closes"). A send that doesn't go out
//! is kept in health by what it was sent for as well as the call, so two
//! failures of the same call under the two purposes are two entries.
//!
//! Here the node turns the first two `keep`s away (the keeper briefly out of
//! gas money), one under each purpose; the third, the same call again, goes
//! out, the bill is collected and the vault closed. The call going out under
//! either purpose is the call going out: health says nothing is failing to
//! go out, not for fifteen minutes of chain time after the bill went through.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    STATION, VAULT, WAD, a_spec, answers, node, one_vault, sent, serve, serve_node, start_keeper,
    timers, wait_until,
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bill_that_went_through_is_not_said_not_to_go_out() {
    let start = 1_700_000_005u64;
    let halted_at = start - 10_000;
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
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(&node, STATION, IStation::stoppedCall {}, true);
    answers(&node, STATION, IStation::outsideBandCall {}, false);
    answers(
        &node,
        STATION,
        IStation::valueCall {},
        IStation::valueReturn {
            mark: U256::ZERO,
            exit: U256::ZERO,
        },
    );
    answers(
        &node,
        STATION,
        IStation::unwindCall {
            fraction: U256::from(WAD),
        },
        IStation::unwindReturn {
            sentHome: U256::ZERO,
            blocked: false,
        },
    );
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("closed", 0);
        locked.vars.insert("turned_away", 0);
        locked.on_call = Some(Arc::new(|node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                // A chain of two-second blocks, about one a round.
                node.timestamp += 2;
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
                    Err("WrongState()".to_string())
                });
            }
            None
        }));
        locked.on_send = Some(Arc::new(|node, to, data| {
            if to == VAULT
                && IVault::keepCall::abi_decode(data).is_ok_and(|keep| !keep.total.is_zero())
                && node.vars["turned_away"] < 2
            {
                *node.vars.get_mut("turned_away").unwrap() += 1;
                return Some(
                    json!({"code": -32000, "message": "insufficient funds for gas * price + value"}),
                );
            }
            if to == VAULT && data[..4] == IVault::closeCall::SELECTOR {
                node.vars.insert("closed", 1);
            }
            None
        }));
    }
    let executor = serve(axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || async move {
            axum::Json(json!({
                "total": "1000000",
                "as_of": start - 1,
                "signature": "0x01",
            }))
        }),
    ))
    .await;
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("same-keep-two-purposes", &rpc, &executor);

    assert!(
        wait_until(Duration::from_secs(60), || {
            node.lock().unwrap().vars["closed"] == 1
        })
        .await,
        "setup: the vault never closed"
    );
    assert_eq!(
        node.lock().unwrap().vars["turned_away"],
        2,
        "setup: the node did not turn two keeps away"
    );
    assert_eq!(
        sent::<IVault::keepCall>(&node, VAULT),
        1,
        "setup: the bill's keep did not go through once"
    );
    // A few rounds of the closed vault after it.
    tokio::time::sleep(Duration::from_secs(8)).await;
    let health = keeper.health().await;
    assert!(
        health
            .as_ref()
            .is_some_and(|(status, said)| *status == 200 || !said.contains("not going out")),
        "the bill's keep went through and the vault closed, yet /health still says a send is \
         not going out (and will for fifteen minutes of chain time): {health:?}"
    );
}
