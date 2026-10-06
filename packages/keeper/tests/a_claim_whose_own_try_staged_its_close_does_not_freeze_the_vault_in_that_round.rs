//! An open vault whose station closes through a venue that acts after the
//! transaction, with one claim waiting, unpaid for more than the freeze
//! period. It is tried this round whatever its wait, and the vault frozen if
//! the try leaves it with nothing under way.
//!
//! The keeper's try stages a close for it: the claim is now closing, and is
//! paid when that close is delivered, within its windows. A claim whose
//! close is staged is never frozen for by the keeper in any later round, so
//! it must not be frozen for in the round that staged it either: freezing
//! halts everyone, for a claim the try has just started paying.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::driver::{ExitPath, IDriver};
use chain::bindings::station::{IStation, IStationVault, Stage};
use chain::bindings::vault::{Claim, ClaimState, IVault, VaultState};
use process::{
    DAY, STATION, VAULT, WAD, a_spec, a_tank, answers, name, node, one_vault, sent, serve_node,
    start_keeper, timers, wait_until,
};
use serde_json::json;

const DRIVER: Address = Address::repeat_byte(0x77);

#[tokio::test(flavor = "multi_thread")]
async fn a_claim_whose_own_try_staged_its_close_does_not_freeze_the_vault_in_that_round() {
    // On a slot's boundary, and every round a slot on.
    let start = 1_700_000_000u64;
    let node = node(start);
    one_vault(&node);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(&node, VAULT, IVault::tankCall {}, a_tank(50_000_000));
    answers(&node, VAULT, IStationVault::haltedAtCall {}, 0);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::from(1u64));

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
        for var in ["rounds", "staged_for"] {
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
                    shares: U256::from(250_000_000u64),
                    // Eight days ago, past the week after which the vault
                    // may be frozen.
                    bookedAt: 1_700_000_000 - 8 * DAY,
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
                    stagedAt: if staged { 1_700_000_010 } else { 0 },
                    stagedBlock: if staged { 90 } else { 0 },
                    sweptAt: 0,
                    sweptBlock: 0,
                    window: 1_800,
                    delay: 120,
                    forVault: true,
                })));
            }
            // The close is still inside its settle delay for the whole test.
            if to == STATION && selector == IStation::settleCloseCall::SELECTOR {
                return Some(Err("NotYet()".to_string()));
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
                // One close at a time.
                if node.vars["staged_for"] != 0 {
                    return Some(json!({"code": 3, "message": "execution reverted: NotYet()"}));
                }
                node.vars.insert("staged_for", id);
            }
            None
        }));
    }
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("own-try-staged", &rpc, "http://127.0.0.1:1");

    assert!(
        wait_until(Duration::from_secs(120), || {
            let node = node.lock().unwrap();
            node.vars["staged_for"] != 0 && node.vars["rounds"] >= 6
        })
        .await,
        "no close was staged in six rounds: {}",
        keeper.logged()
    );
    let rounds = node.lock().unwrap().vars["rounds"];
    let freezes = sent::<IVault::freezeCall>(&node, VAULT);
    assert_eq!(
        freezes, 0,
        "in {rounds} rounds the keeper froze the vault {freezes} time(s) for a claim whose close \
         its own try had just staged"
    );
}
