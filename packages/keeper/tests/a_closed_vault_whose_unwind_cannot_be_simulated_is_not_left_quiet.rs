//! A closed vault, past its grace and its last bill's window (an empty
//! tank, so there is no bill), its station stopped and holding $60 at the
//! mark that its full unwind would bring nothing home for. Whether the
//! unwind empties the station is known only from a simulation of what it
//! leaves, and every such simulation fails (an internal error, a state
//! override unavailable).
//!
//! A failed simulation says nothing about whether the unwind is worth
//! sending, so the vault is not left quiet (looked at once a day) on it: it
//! stays in the round, the simulation is made again each slot, and after
//! thirty failures in a row the vault is named in the keeper's health.

mod process;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    STATION, VAULT, WAD, a_spec, a_tank, answers, name, node, one_vault, serve_node,
    start_keeper_holding, timers, wait_until,
};
use serde_json::{Value, json};

/// Whether a call's code, as creation code or an overridden contract's,
/// names the station: the keeper's simulation of what its unwind leaves.
fn probes_the_station(params: &Value) -> bool {
    let names = |hex: Option<&str>| {
        hex.and_then(|hex| alloy::hex::decode(hex).ok())
            .is_some_and(|code| code.windows(20).any(|at| at == STATION.as_slice()))
    };
    let created = params[0].get("to").is_none_or(Value::is_null)
        && names(
            params[0]
                .get("input")
                .or_else(|| params[0].get("data"))
                .and_then(Value::as_str),
        );
    let overridden = params
        .get(2)
        .and_then(Value::as_object)
        .is_some_and(|overrides| {
            overrides
                .values()
                .any(|account| names(account["code"].as_str()))
        });
    created || overridden
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_vault_whose_unwind_cannot_be_simulated_is_not_left_quiet() {
    let start = 1_700_000_005u64;
    let node = node(start);
    one_vault(&node);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(&node, VAULT, IVault::tankCall {}, a_tank(0));
    answers(&node, VAULT, IStationVault::haltedAtCall {}, start - 5_000);
    answers(&node, VAULT, IStationVault::haltGraceCall {}, 600);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
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
            mark: U256::from(60_000_000u64),
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
    let probes = Arc::new(AtomicU64::new(0));
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("rounds", 0);
        // A chain-minute a round: a slot and more each round.
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
        let counted = probes.clone();
        locked.on_request = Some(Arc::new(move |method, params: &Value| {
            if method != "eth_call" || !probes_the_station(params) {
                return None;
            }
            // Counted once per check: the creation-code form is tried first.
            if params.get(2).is_none_or(Value::is_null) {
                counted.fetch_add(1, Ordering::SeqCst);
            }
            Some(
                if params.get(2).is_some_and(|overrides| !overrides.is_null()) {
                    json!({"code": -32602, "message": "invalid params: state override unavailable"})
                } else {
                    json!({"code": -32603, "message": "internal error"})
                },
            )
        }));
    }
    let rpc = serve_node(node.clone()).await;
    // The window is judged on the chain's clock, which runs a minute a round.
    let keeper = start_keeper_holding("closed-unprobed", &rpc, "http://127.0.0.1:1", 0);

    assert!(
        wait_until(Duration::from_secs(200), || {
            node.lock().unwrap().vars["rounds"] >= 45
        })
        .await,
        "the keeper did not run forty-five rounds"
    );
    let tried = probes.load(Ordering::SeqCst);
    let health = keeper.health_until(Duration::from_secs(5), |_| true).await;
    let said = health.as_ref().is_some_and(|(status, said)| {
        *status == 503
            && said.contains("unwinds not simulated")
            && said.contains(&format!("{VAULT:?}"))
    });
    assert!(
        tried >= 30 && said,
        "45 slots of a closed vault whose stopped station's unwind can't be simulated: the \
         simulation was tried {tried} times (the vault went quiet for a day) and the keeper's \
         health {} the vault: {health:?}",
        if said { "names" } else { "never names" }
    );
}
