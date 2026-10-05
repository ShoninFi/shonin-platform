//! A vault still activating past its window: the keeper sends `cancel`, and
//! the node turns it away once (a gateway's rate limit), while a stranger's
//! `cancel` lands in the same block. From then on the vault is cancelled,
//! with nothing left to do, and the keeper never sends `cancel` again. The
//! rounds no longer want it, so it is no longer said not to go out: the
//! keeper's health answers as up.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::sol_types::SolCall;
use chain::bindings::station::IStation;
use chain::bindings::vault::{IVault, VaultState};
use process::{
    STATION, VAULT, a_spec, answers, called, node, one_vault, serve_node, start_keeper, timers,
    wait_until,
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn a_one_off_send_no_longer_wanted_does_not_hold_health_down() {
    let node = node(1_700_000_005);
    one_vault(&node);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(&node, VAULT, IVault::createdAtCall {}, 1_600_000_000);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("cancelled", 0);
        locked.on_call = Some(Arc::new(|node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                // An hour of chain time a round, as the devnet runs.
                node.timestamp += 3_600;
                let state = if node.vars["cancelled"] == 1 {
                    VaultState::Cancelled
                } else {
                    VaultState::Activating
                };
                return Some(Ok(IVault::stateCall::abi_encode_returns(&state)));
            }
            None
        }));
        locked.on_send = Some(Arc::new(|node, to, data| {
            if to == VAULT && data[..4] == IVault::cancelCall::SELECTOR {
                node.vars.insert("cancelled", 1);
                return Some(json!({"code": -32005, "message": "request rate limited"}));
            }
            None
        }));
    }
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("one-off", &rpc, "http://127.0.0.1:1");

    // The cancel was turned away, and heard of.
    assert!(
        wait_until(Duration::from_secs(30), || {
            node.lock().unwrap().vars["cancelled"] == 1
        })
        .await
    );
    assert!(
        wait_until(Duration::from_secs(10), || keeper
            .logged()
            .contains("the keeper's send did not go out"))
        .await
    );
    assert!(
        wait_until(Duration::from_secs(40), || {
            called::<IVault::stateCall>(&node, VAULT) >= 6
        })
        .await
    );
    let health = keeper.health().await;
    assert_eq!(
        health.as_ref().map(|(status, _)| *status),
        Some(200),
        "the vault was cancelled rounds ago and nothing is failing, yet /health answered {health:?}"
    );
}
