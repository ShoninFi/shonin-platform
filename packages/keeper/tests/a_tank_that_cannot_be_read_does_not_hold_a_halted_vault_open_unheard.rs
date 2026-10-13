//! A halted vault, past its grace, that `close()` would close at once. Its
//! `tank()` can't be read: every read of it fails with a node error (here
//! `header not found`; a vault from an earlier factory whose `Tank` layout
//! the keeper's binding no longer decodes would fail the same way, for
//! good).
//!
//! The close is held for its bill; when the hold ends, its last try can't
//! be made without the tank, so it is made again, but for no longer than the
//! hold again. Then the bill is written off and the close goes: depositors'
//! money is not held for good for a bill that can't be collected. Meanwhile
//! the keeper's health says the tank can't be read, and the warn line says
//! so once, not every round.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    STATION, VAULT, WAD, a_spec, answers, node, one_vault, sent, serve, serve_node,
    start_keeper_holding, timers, wait_until,
};
use serde_json::{Value, json};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tank_that_cannot_be_read_does_not_hold_a_halted_vault_open_unheard() {
    let start = 1_700_000_005u64;
    let halted_at = start - 10_000;
    let node = node(start);
    one_vault(&node);
    answers(&node, VAULT, IVault::timersCall {}, timers());
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
        locked.vars.insert("closed_at", 0);
        locked.on_call = Some(Arc::new(|node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                // Two minutes of chain time a round.
                node.timestamp += 120;
                let state = if node.vars["closed"] == 1 {
                    VaultState::Closed
                } else {
                    VaultState::Killed
                };
                return Some(Ok(IVault::stateCall::abi_encode_returns(&state)));
            }
            // The close would go through: a valuation after the halt found
            // the station empty and everything is settled.
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
            if to == VAULT && data[..4] == IVault::closeCall::SELECTOR {
                node.vars.insert("closed", 1);
                let at = node.timestamp;
                node.vars.insert("closed_at", at);
            }
            None
        }));
        // Every read of the tank fails at the node.
        locked.on_request = Some(Arc::new(|method: &str, params: &Value| {
            if method != "eth_call" {
                return None;
            }
            let tx = &params[0];
            let input = tx
                .get("input")
                .or_else(|| tx.get("data"))
                .and_then(Value::as_str)
                .unwrap_or("0x");
            let data = alloy::hex::decode(input).unwrap_or_default();
            (data.len() >= 4 && data[..4] == IVault::tankCall::SELECTOR)
                .then(|| json!({"code": -32000, "message": "header not found"}))
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
    // The keeper's own clock is let off the hold, so only chain time counts.
    let keeper = start_keeper_holding("tank-unread-hold", &rpc, &executor, 0);

    let health = keeper
        .health_saying(Duration::from_secs(60), "tanks unreadable")
        .await;
    assert!(
        health
            .as_ref()
            .is_some_and(|(_, said)| said.contains("tanks unreadable")),
        "the vault's tank can't be read round after round, yet /health answered {health:?}"
    );
    assert!(
        wait_until(Duration::from_secs(150), || {
            node.lock().unwrap().vars["closed"] == 1
        })
        .await,
        "the halted vault was never closed: its hold runs on for want of a tank the keeper \
         can't read (chain time now {} past the start)",
        node.lock().unwrap().timestamp - start,
    );
    // The hold, then its last tries for as long again, in chain time: the
    // first round holds the close two minutes after the start.
    let closed_at = node.lock().unwrap().vars["closed_at"];
    assert!(
        closed_at >= start + 120 + 2 * 900,
        "the close went {}s after the start, before the hold and its last tries were over",
        closed_at - start
    );
    assert_eq!(
        sent::<IVault::keepCall>(&node, VAULT),
        0,
        "setup: a keep went out without the tank"
    );
    let warned = keeper.logged().matches("round after round").count();
    assert_eq!(
        warned, 1,
        "the tank that can't be read is said once, then at most every ten minutes"
    );
}
