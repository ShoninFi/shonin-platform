//! A halted vault past its grace, no Closer, its stopped station holding a
//! levered account worth $60 at the mark and nothing at the bid: its full
//! unwind brings nothing home and empties the station, which is what lets
//! the vault close. Whether it empties the station is known only from a
//! simulation of the unwind and the value after it, which runs code that is
//! never deployed: as creation code, or as an overridden contract's code.
//!
//! Here the chain's node runs neither, as a gateway that won't run
//! `eth_call` without a contract to call, or with a state override, does.
//! The keeper sends nothing it hasn't simulated, so the chain is not kept at
//! all: nothing is read or sent for its vaults, and that is said at warn and
//! in the keeper's health, which answers 503. The check is made again, ever
//! less often; once the node simulates, the chain is kept.

mod process;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    STATION, VAULT, WAD, a_spec, a_tank, answers, name, node, one_vault, serve_node, start_keeper,
    timers, wait_until,
};
use serde_json::{Value, json};

#[tokio::test(flavor = "multi_thread")]
async fn a_chain_whose_node_cannot_simulate_is_not_kept_and_is_said() {
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
    let simulates = Arc::new(AtomicBool::new(false));
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
        // Neither a call without a contract nor one with a state override,
        // until the node is told to simulate.
        let simulating = simulates.clone();
        locked.on_request = Some(Arc::new(move |method, params: &Value| {
            let unsupported = method == "eth_call"
                && !simulating.load(Ordering::SeqCst)
                && (params[0].get("to").is_none_or(Value::is_null)
                    || params.get(2).is_some_and(|overrides| !overrides.is_null()));
            unsupported.then(|| json!({"code": -32602, "message": "unsupported call"}))
        }));
    }
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("cannot-simulate", &rpc, "http://127.0.0.1:1");

    assert!(
        wait_until(Duration::from_secs(20), || keeper
            .logged()
            .contains("this chain's node can't simulate"))
        .await,
        "the simulation refusal was never reached: {}",
        keeper.logged()
    );
    tokio::time::sleep(Duration::from_secs(8)).await;
    let health = keeper.health().await;
    let (rounds, sent) = {
        let locked = node.lock().unwrap();
        (locked.vars["rounds"], locked.sent.len())
    };
    let logged = keeper.logged();
    let warned = logged
        .lines()
        .filter(|line| line.contains("WARN") && line.contains("can't simulate"))
        .count();
    assert_eq!(
        (rounds, sent),
        (0, 0),
        "a chain whose node can't simulate was kept: {rounds} rounds, {sent} sends"
    );
    assert!(
        health
            .as_ref()
            .is_some_and(|(status, said)| *status == 503 && said.contains("base (can't simulate)")),
        "the keeper's health did not say the chain can't simulate: {health:?}"
    );
    assert_eq!(warned, 1, "said at warn {warned} times: {logged}");

    // The node simulates from now on: the chain is kept at the next check.
    simulates.store(true, Ordering::SeqCst);
    assert!(
        wait_until(Duration::from_secs(30), || {
            node.lock().unwrap().vars["rounds"] >= 1
        })
        .await,
        "the chain was not kept once its node could simulate"
    );
    let health = keeper.health_until(Duration::from_secs(5), |_| true).await;
    assert!(
        health
            .as_ref()
            .is_some_and(|(_, said)| !said.contains("can't simulate")),
        "the keeper's health still says the chain can't simulate: {health:?}"
    );
}
