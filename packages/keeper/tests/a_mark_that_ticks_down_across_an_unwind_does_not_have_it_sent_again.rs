//! A halted vault past its grace, its station stopped and holding a $100
//! remnant the venue values at the mark and no unwind ever sells: nothing
//! comes home, it is never blocked, and the station is left as it was. The
//! mark is a live one, which over each unwind's block ticks down two cents
//! and is back by the next slot. Whether to send an unwind is judged by its
//! simulation alone, never by how the mark moved: in twenty slots none is
//! paid for more than once.

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

#[tokio::test(flavor = "multi_thread")]
async fn a_mark_that_ticks_down_across_an_unwind_does_not_have_it_sent_again() {
    let mark = 100_000_000u64;
    let drift = 20_000u64;
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
        locked.vars.insert("mark", mark);
        locked.vars.insert("rounds", 0);
        locked.on_call = Some(Arc::new(move |node, to, data| {
            // Each round starts with the vault's state: a slot later, the
            // mark back where it was.
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 10;
                *node.vars.get_mut("rounds").unwrap() += 1;
                node.vars.insert("mark", mark);
                return Some(Ok(IVault::stateCall::abi_encode_returns(
                    &VaultState::Killed,
                )));
            }
            if to == STATION && data[..4] == IStation::valueCall::SELECTOR {
                return Some(Ok(IStation::valueCall::abi_encode_returns(
                    &IStation::valueReturn {
                        mark: U256::from(node.vars["mark"]),
                        exit: U256::ZERO,
                    },
                )));
            }
            // The simulation of the unwind and the value after it: nothing
            // home, not blocked, and the remnant where it was, but for the
            // market's own tick.
            if to == Address::ZERO {
                let after = U256::from(node.vars["mark"] - drift);
                let mut returned = vec![0u8; 64];
                returned.extend_from_slice(&after.to_be_bytes::<32>());
                return Some(Ok(returned));
            }
            None
        }));
        // The unwind's block: the market ticks.
        locked.on_send = Some(Arc::new(move |node, to, data| {
            if to == STATION && data[..4] == IStation::unwindCall::SELECTOR {
                let now = node.vars["mark"];
                node.vars.insert("mark", now - drift);
            }
            None
        }));
    }
    let rpc = serve_node(node.clone()).await;
    let _keeper = start_keeper("mark-ticking", &rpc, "http://127.0.0.1:1");
    assert!(
        wait_until(Duration::from_secs(90), || {
            node.lock().unwrap().vars["rounds"] >= 21
        })
        .await,
        "the keeper did not run twenty rounds"
    );
    let unwinds = sent::<IStation::unwindCall>(&node, STATION);
    assert!(
        unwinds <= 1,
        "{unwinds} paid unwinds in twenty slots for a remnant no unwind ever sold"
    );
}
