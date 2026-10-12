//! An open vault whose station closes through a venue that acts after the
//! transaction, with two claims waiting. The first is large: its closing
//! orders never fill, so each of its staged closes ends, once its window has
//! passed, with nothing of it paid, and the claim waits again. The second is
//! small and could be closed.
//!
//! The vault stages one close at a time, and wants a priced valuation
//! between two. So each time the first claim's close ends, whichever claim
//! the keeper tries first after the next valuation takes the stage for
//! another window, and every other claim is refused meanwhile. The keeper
//! puts the claim a close was last staged for at the back of the turn, so
//! the second claim is tried before the first is tried again: a claim whose
//! close keeps ending unpaid never keeps another from its turn until the
//! vault can be frozen for it.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::driver::{ExitPath, IDriver};
use chain::bindings::station::{IStation, IStationVault, Stage};
use chain::bindings::vault::{Claim, ClaimState, IVault, VaultState};
use process::{
    STATION, VAULT, WAD, a_spec, a_tank, answers, name, node, one_vault, serve_node, start_keeper,
    timers, wait_until,
};
use serde_json::json;

const DRIVER: Address = Address::repeat_byte(0x77);

/// How many of the keeper's rounds (a slot each) a staged close waits before
/// it lapses unproved.
const WINDOW_ROUNDS: u64 = 2;

fn refused() -> serde_json::Value {
    json!({"code": 3, "message": "execution reverted: NotYet()"})
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claim_whose_close_keeps_ending_unpaid_does_not_keep_the_next_claim_from_its_turn() {
    // On a slot's boundary, and every round a slot on: each round's
    // valuation lands inside its window.
    let start = 1_700_000_000u64;
    let node = node(start);
    one_vault(&node);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(&node, VAULT, IVault::tankCall {}, a_tank(50_000_000));
    answers(&node, VAULT, IStationVault::haltedAtCall {}, 0);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::from(2u64));

    let driver = name("hyperliquid");
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![driver]));
    answers(
        &node,
        STATION,
        IStation::approvedKeyCall { driver },
        Address::ZERO,
    );
    answers(
        &node,
        STATION,
        IStation::pinnedDriverCall { driver },
        DRIVER,
    );
    answers(&node, STATION, IStation::stoppedCall {}, false);
    answers(&node, STATION, IStation::outsideBandCall {}, false);
    answers(&node, STATION, IStation::heldInCall { driver }, vec![]);
    answers(&node, DRIVER, IDriver::exitPathCall {}, ExitPath::Delayed);
    answers(&node, DRIVER, IDriver::nameCall {}, driver);
    {
        let mut locked = node.lock().unwrap();
        for var in [
            "rounds",
            "staged_for",
            "staged_in",
            "value_first",
            "first_staged",
            "second_staged",
            "lapsed",
        ] {
            locked.vars.insert(var, 0);
        }
        locked.on_call = Some(Arc::new(move |node, to, data| {
            let selector = &data[..4];
            if to == VAULT && selector == IVault::stateCall::SELECTOR {
                node.timestamp += 10;
                *node.vars.get_mut("rounds").unwrap() += 1;
                return Some(Ok(IVault::stateCall::abi_encode_returns(&VaultState::Open)));
            }
            if to == VAULT && selector == IVault::claimAtCall::SELECTOR {
                let id: u64 = IVault::claimAtCall::abi_decode(data)
                    .ok()?
                    .claimId
                    .try_into()
                    .ok()?;
                let state = if node.vars["staged_for"] == id {
                    ClaimState::Closing
                } else {
                    ClaimState::Waiting
                };
                return Some(Ok(IVault::claimAtCall::abi_encode_returns(&Claim {
                    owner: Address::repeat_byte(0x40 + id as u8),
                    // The first owns a quarter of the vault, the second a
                    // thousandth; both booked a minute ago.
                    shares: U256::from(if id == 1 { 250_000_000u64 } else { 1_000_000 }),
                    bookedAt: 1_700_000_000 - 60,
                    lastAttemptAt: 0,
                    state,
                    paid: U256::ZERO,
                })));
            }
            if to == STATION && selector == IStation::stageCall::SELECTOR {
                let staged = node.vars["staged_for"] != 0;
                return Some(Ok(IStation::stageCall::abi_encode_returns(&Stage {
                    fraction: U256::from(if staged { WAD / 4 } else { 0 }),
                    owed: U256::ZERO,
                    estimate: U256::from(if staged { 25_000_000_000u64 } else { 0 }),
                    fetched: U256::ZERO,
                    cost: U256::ZERO,
                    part: U256::ZERO,
                    arrivedAt: U256::ZERO,
                    stagedAt: if staged { node.timestamp - 10 } else { 0 },
                    stagedBlock: if staged { 90 } else { 0 },
                    sweptAt: 0,
                    sweptBlock: 0,
                    window: 300,
                    delay: 0,
                    provedBy: u64::MAX,
                    forVault: true,
                })));
            }
            // Never proved: it can only lapse, once its window has passed.
            if to == STATION && selector == IStation::settleCloseCall::SELECTOR {
                let over = node.vars["staged_for"] != 0
                    && node.vars["rounds"] >= node.vars["staged_in"] + WINDOW_ROUNDS;
                return Some(if over {
                    Ok(Vec::new())
                } else {
                    Err("NotYet()".to_string())
                });
            }
            None
        }));
        locked.on_send = Some(Arc::new(|node, to, data| {
            let selector = &data[..4];
            if to == VAULT && selector == IVault::settleClaimCall::SELECTOR {
                let id: u64 = IVault::settleClaimCall::abi_decode(data)
                    .ok()?
                    .claimId
                    .try_into()
                    .ok()?;
                // One close at a time, and a valuation between two.
                if node.vars["staged_for"] != 0 || node.vars["value_first"] == 1 {
                    return Some(refused());
                }
                let rounds = node.vars["rounds"];
                node.vars.insert("staged_for", id);
                node.vars.insert("staged_in", rounds);
                let counted = if id == 1 {
                    "first_staged"
                } else {
                    "second_staged"
                };
                *node.vars.get_mut(counted).unwrap() += 1;
                // The second claim's close would be proved and delivered:
                // the test ends when it is staged, so nothing more is
                // scripted for it.
            }
            if to == STATION && selector == IStation::settleCloseCall::SELECTOR {
                if node.vars["staged_for"] == 0 {
                    return Some(refused());
                }
                node.vars.insert("staged_for", 0);
                node.vars.insert("value_first", 1);
                *node.vars.get_mut("lapsed").unwrap() += 1;
            }
            if to == VAULT && selector == IVault::recordValuationCall::SELECTOR {
                node.vars.insert("value_first", 0);
            }
            None
        }));
    }
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("close-keeps-lapsing", &rpc, "http://127.0.0.1:1");

    // Until the first claim's close has lapsed six times, or the second
    // claim has had a close staged.
    assert!(
        wait_until(Duration::from_secs(240), || {
            let node = node.lock().unwrap();
            node.vars["lapsed"] >= 6 || node.vars["second_staged"] >= 1
        })
        .await,
        "the first claim's close did not lapse six times: {}",
        keeper.logged()
    );
    let (first, second, lapsed, rounds) = {
        let node = node.lock().unwrap();
        (
            node.vars["first_staged"],
            node.vars["second_staged"],
            node.vars["lapsed"],
            node.vars["rounds"],
        )
    };
    assert!(
        second >= 1,
        "in {rounds} rounds the first claim's close was staged {first} times and lapsed {lapsed} \
         times, each time staged again at once, and the second claim never had a turn"
    );
}
