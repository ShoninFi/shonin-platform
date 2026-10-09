//! An open vault whose station closes through a venue that acts after the
//! transaction, with two claims waiting. The first is large: each close
//! staged for it ends, a round later, with nothing of it paid, and the claim
//! waits again with its shares. The second is small: the cash covers it, so
//! its try queues it and the next priced valuation pays it.
//!
//! The keeper puts the claim a close was last staged for at the back of the
//! turn. That must only change the order: once the second claim is paid, the
//! first is the one claim left, and it must go on being tried after every
//! valuation, as it would be if it were alone. A keeper that passes over
//! every paid claim it meets before the first unpaid one in the turn, and
//! starts the next round after them, never looks at the first claim again:
//! it is neither tried nor, a week on, frozen for.

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

fn refused() -> serde_json::Value {
    json!({"code": 3, "message": "execution reverted: NotYet()"})
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claim_tried_after_the_others_is_still_tried_once_they_are_paid() {
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
            "second_queued",
            "second_paid",
            "paid_in",
            "first_staged",
            "first_staged_since",
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
                let state = if id == 2 && node.vars["second_paid"] == 1 {
                    ClaimState::Paid
                } else if id == 2 && node.vars["second_queued"] == 1 {
                    ClaimState::Queued
                } else if node.vars["staged_for"] == id {
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
                    window: 10,
                    delay: 0,
                    provedBy: u64::MAX,
                    forVault: true,
                })));
            }
            // The first claim's close never fills: a round after it was
            // placed its window is over, and it ends with nothing paid.
            if to == STATION && selector == IStation::settleCloseCall::SELECTOR {
                let over =
                    node.vars["staged_for"] != 0 && node.vars["rounds"] > node.vars["staged_in"];
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
                if id == 2 {
                    // Only a waiting claim is tried; the cash covers this
                    // one, so it is queued for the next priced valuation.
                    if node.vars["second_queued"] == 1 || node.vars["second_paid"] == 1 {
                        return Some(
                            json!({"code": 3, "message": "execution reverted: NotAllowed()"}),
                        );
                    }
                    node.vars.insert("second_queued", 1);
                    return None;
                }
                let rounds = node.vars["rounds"];
                node.vars.insert("staged_for", id);
                node.vars.insert("staged_in", rounds);
                *node.vars.get_mut("first_staged").unwrap() += 1;
                if node.vars["second_paid"] == 1 {
                    *node.vars.get_mut("first_staged_since").unwrap() += 1;
                }
            }
            if to == STATION && selector == IStation::settleCloseCall::SELECTOR {
                if node.vars["staged_for"] == 0 {
                    return Some(refused());
                }
                node.vars.insert("staged_for", 0);
                node.vars.insert("value_first", 1);
            }
            if to == VAULT && selector == IVault::recordValuationCall::SELECTOR {
                // Priced only with no close in progress; a priced valuation
                // pays the queued claim.
                if node.vars["staged_for"] == 0 {
                    node.vars.insert("value_first", 0);
                    if node.vars["second_queued"] == 1 && node.vars["second_paid"] == 0 {
                        let rounds = node.vars["rounds"];
                        node.vars.insert("second_paid", 1);
                        node.vars.insert("paid_in", rounds);
                    }
                }
            }
            None
        }));
    }
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("claim-tried-last-forgotten", &rpc, "http://127.0.0.1:1");

    // Until the first claim has been tried three times since the second was
    // paid, or forty rounds (each a slot, with a priced valuation whenever
    // no close is in progress) have passed since.
    assert!(
        wait_until(Duration::from_secs(240), || {
            let node = node.lock().unwrap();
            node.vars["second_paid"] == 1
                && (node.vars["first_staged_since"] >= 3
                    || node.vars["rounds"] >= node.vars["paid_in"] + 40)
        })
        .await,
        "the second claim was never paid, or the keeper stopped its rounds: {}",
        keeper.logged()
    );
    let (first, since, paid_in, rounds) = {
        let node = node.lock().unwrap();
        (
            node.vars["first_staged"],
            node.vars["first_staged_since"],
            node.vars["paid_in"],
            node.vars["rounds"],
        )
    };
    assert!(
        since >= 3,
        "the second claim was paid in round {paid_in}; in the {} rounds since, each with a priced \
         valuation between two closes, the first claim (still unpaid, a close staged for it \
         {first} times in all) was tried {since} time(s) and then never again",
        rounds - paid_in
    );
}
