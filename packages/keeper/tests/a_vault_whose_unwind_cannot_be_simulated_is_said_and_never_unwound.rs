//! A halted vault past its grace, no Closer, its stopped station holding a
//! levered account worth $60 at the mark and nothing at the bid: its full
//! unwind brings nothing home and empties the station, which is what lets
//! the vault close. Whether it empties the station is known only from a
//! simulation of what the unwind leaves.
//!
//! The chain's node simulates (the keeper's check when the chain connects
//! goes through), but every simulation of this station's unwind fails, in
//! either form, with errors that say nothing either way: an internal error,
//! a "state override unavailable". The keeper never sends the unwind
//! without that simulation; it makes it again every slot, and once it has
//! failed thirty slots in a row, the keeper says so at warn and in its
//! health, naming the vault.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    STATION, VAULT, WAD, a_spec, a_tank, answers, name, node, one_vault, sent, serve_node,
    start_keeper, timers, wait_until,
};
use serde_json::{Value, json};

/// Whether a call's code, as creation code or an overridden contract's,
/// names the station: the keeper's simulation of its unwind.
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
async fn a_vault_whose_unwind_cannot_be_simulated_is_said_and_never_unwound() {
    let node = node(1_700_000_005);
    one_vault(&node);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(&node, VAULT, IVault::tankCall {}, a_tank(0));
    answers(&node, VAULT, IStationVault::haltedAtCall {}, 1_699_000_000);
    answers(&node, VAULT, IStationVault::haltGraceCall {}, 600);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
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
    answers(&node, STATION, IStation::outsideBandCall {}, false);
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
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("rounds", 0);
        // A slot a round.
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 10;
                *node.vars.get_mut("rounds").unwrap() += 1;
                return Some(Ok(IVault::stateCall::abi_encode_returns(
                    &VaultState::Killed,
                )));
            }
            None
        }));
        locked.on_request = Some(Arc::new(|method, params: &Value| {
            if method != "eth_call" || !probes_the_station(params) {
                return None;
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
    let keeper = start_keeper("unwind-cannot-be-simulated", &rpc, "http://127.0.0.1:1");

    let said = |health: &Option<(u16, String)>| {
        health.as_ref().is_some_and(|(status, said)| {
            *status == 503
                && said.contains("unwinds not simulated")
                && said.contains(&format!("{VAULT:?}"))
        })
    };
    // Before thirty slots it is not said: a simulation that fails now and
    // then is a node's hiccup.
    assert!(
        wait_until(Duration::from_secs(60), || {
            node.lock().unwrap().vars["rounds"] >= 10
        })
        .await,
        "the keeper did not run ten rounds"
    );
    let early = keeper.health().await;
    assert!(!said(&early), "said after ten slots: {early:?}");

    assert!(
        wait_until(Duration::from_secs(150), || {
            node.lock().unwrap().vars["rounds"] >= 34
        })
        .await,
        "the keeper did not run thirty-four rounds"
    );
    let rounds = node.lock().unwrap().vars["rounds"];
    let unwinds = sent::<IStation::unwindCall>(&node, STATION);
    let health = keeper.health_until(Duration::from_secs(5), |_| true).await;
    let logged = keeper.logged();
    let warned = logged
        .lines()
        .filter(|line| line.contains("WARN") && line.contains("can't be simulated"))
        .count();
    assert_eq!(
        unwinds, 0,
        "{unwinds} unwinds sent in {rounds} slots without a simulation of what they leave"
    );
    assert!(
        said(&health),
        "{rounds} slots of failed simulations, and the keeper's health does not name the \
         vault: {health:?}"
    );
    assert_eq!(warned, 1, "said at warn {warned} times: {logged}");
}
