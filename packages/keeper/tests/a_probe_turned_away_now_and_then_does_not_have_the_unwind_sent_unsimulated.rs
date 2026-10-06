//! A halted vault past its grace, no Closer, its stopped station holding a
//! $60 remnant no unwind ever sells. The node simulates what the unwind
//! leaves (the probe says the station is left as it was, so the unwind is
//! not worth sending) except now and then, when it turns the probe away (a
//! gateway's rate limit: every other check here).
//!
//! A check whose simulation is turned away sends nothing, and the next one
//! simulates again: no unwind is paid for, and since the simulation never
//! fails many times in a row, the keeper's health never names the vault.

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

#[tokio::test(flavor = "multi_thread")]
async fn a_probe_turned_away_now_and_then_does_not_have_the_unwind_sent_unsimulated() {
    let mark = 60_000_000u64;
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
            mark: U256::from(mark),
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
    let rounds = Arc::new(std::sync::atomic::AtomicU64::new(0));
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("rounds", 0);
        let counted = rounds.clone();
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 300;
                *node.vars.get_mut("rounds").unwrap() += 1;
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Some(Ok(IVault::stateCall::abi_encode_returns(
                    &VaultState::Killed,
                )));
            }
            // The probe, run as creation code: the station left as it was.
            if to == Address::ZERO {
                let mut returned = Vec::new();
                returned.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
                returned.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
                returned.extend_from_slice(&U256::from(mark).to_be_bytes::<32>());
                return Some(Ok(returned));
            }
            None
        }));
        // On odd rounds the node turns away calls without a contract and
        // calls with an override: a rate limit, not a lack of support.
        let refusing = rounds.clone();
        locked.on_request = Some(Arc::new(move |method, params: &Value| {
            let probe = method == "eth_call"
                && (params[0].get("to").is_none_or(Value::is_null)
                    || params.get(2).is_some_and(|overrides| !overrides.is_null()));
            let odd = refusing.load(std::sync::atomic::Ordering::SeqCst) % 2 == 1;
            (probe && odd).then(|| json!({"code": -32005, "message": "rate limited"}))
        }));
    }
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("flaky-probe", &rpc, "http://127.0.0.1:1");
    // Each turned-away probe is asked again a few times before the check
    // gives up on it (a rate limit, as the chain client treats any read), so
    // the rounds take longer than they would.
    assert!(
        wait_until(Duration::from_secs(300), || {
            node.lock().unwrap().vars["rounds"] >= 41
        })
        .await,
        "the keeper did not run forty rounds"
    );
    let unwinds = sent::<IStation::unwindCall>(&node, STATION);
    let health = keeper.health_until(Duration::from_secs(5), |_| true).await;
    assert_eq!(
        unwinds, 0,
        "{unwinds} paid unwinds in 40 checks of a remnant no unwind ever moves, the probe \
         rate limited on every other one: a rate limit was taken as a node that can't simulate"
    );
    assert!(
        health
            .as_ref()
            .is_some_and(|(_, said)| !said.contains(&format!("{VAULT:?}"))),
        "the keeper's health names the vault: {health:?}"
    );
}
