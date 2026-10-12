//! A halted vault past its grace, its stopped station trading through one
//! driver whose venue acts after the transaction, with positions left to
//! close and the venue reading the same every round (a station nobody posts
//! for any more). The keeper's account can't pay for gas for a while: every
//! unwind it tries is turned away by the node and never reaches the chain.
//!
//! An unwind that never went out has changed nothing because nothing was
//! tried, not because the venue has nothing left to give. Once the keeper's
//! account is funded again the station must still be unwound: the keeper
//! must not have counted the sends that never went out as unwinds that
//! found the venue unchanged, and stopped for good.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, Bytes, I256, U256};
use alloy::sol_types::SolCall;
use chain::bindings::driver::{Call, ExitPath, IDelayedDriver, IDriver};
use chain::bindings::station::{IStation, IStationVault, Stage};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    STATION, VAULT, WAD, a_spec, a_tank, answers, name, node, one_vault, sent, serve_node,
    start_keeper, timers, wait_until,
};

const DRIVER: Address = Address::repeat_byte(0x77);
const VENUE: Address = Address::repeat_byte(0x88);

/// A halted vault past its grace with no Closer, its station stopped, one
/// driver whose venue acts after the transaction, nothing staged, nothing
/// idle at the station: its unwind brings nothing home in its own
/// transaction.
fn a_stopped_delayed_station(node: &process::Shared) {
    one_vault(node);
    answers(node, VAULT, IVault::timersCall {}, timers());
    answers(node, VAULT, IVault::tankCall {}, a_tank(0));
    answers(node, VAULT, IStationVault::haltedAtCall {}, 1_699_000_000);
    answers(node, VAULT, IStationVault::haltGraceCall {}, 600);
    answers(node, VAULT, IVault::homeStationCall {}, STATION);
    answers(node, VAULT, IStationVault::unsettledCall {}, false);
    answers(node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(node, VAULT, IVault::claimCountCall {}, U256::ZERO);

    let driver = name("venue");
    answers(node, STATION, IStation::specCall {}, a_spec(vec![driver]));
    answers(
        node,
        STATION,
        IStation::approvedKeyCall { driver },
        Address::ZERO,
    );
    answers(node, STATION, IStation::pinnedDriverCall { driver }, DRIVER);
    answers(node, STATION, IStation::stoppedCall {}, true);
    answers(node, STATION, IStation::outsideBandCall {}, false);
    answers(node, STATION, IStation::stoppedBandCall {}, 100);
    answers(node, STATION, IStation::heldInCall { driver }, vec![]);
    answers(
        node,
        STATION,
        IStation::stageCall {},
        Stage {
            fraction: U256::ZERO,
            owed: U256::ZERO,
            estimate: U256::ZERO,
            fetched: U256::ZERO,
            cost: U256::ZERO,
            part: U256::ZERO,
            arrivedAt: U256::ZERO,
            stagedAt: 0,
            stagedBlock: 0,
            sweptAt: 0,
            sweptBlock: 0,
            window: 0,
            delay: 0,
            provedBy: u64::MAX,
            forVault: false,
        },
    );
    // Forty thousand dollars at the venue, none at the station.
    answers(
        node,
        STATION,
        IStation::valueCall {},
        IStation::valueReturn {
            mark: U256::from(40_000_000_000u64),
            exit: U256::from(40_000_000_000u64),
        },
    );
    answers(
        node,
        STATION,
        IStation::unwindCall {
            fraction: U256::from(WAD),
        },
        IStation::unwindReturn {
            sentHome: U256::ZERO,
            blocked: true,
        },
    );
    answers(node, DRIVER, IDriver::exitPathCall {}, ExitPath::Delayed);
    answers(node, DRIVER, IDriver::nameCall {}, driver);
    answers(
        node,
        DRIVER,
        IDelayedDriver::exposureCall { station: STATION },
        vec![I256::try_from(5_000i64).unwrap()],
    );
}

/// One call to the venue whose data is `tag` and `round`.
fn a_call(tag: u8, round: u64) -> Call {
    let mut data = vec![tag];
    data.extend_from_slice(&round.to_be_bytes());
    Call {
        target: VENUE,
        value: U256::ZERO,
        data: Bytes::from(data),
    }
}

/// The rounds in which the node turns every unwind away.
const UNFUNDED_ROUNDS: u64 = 12;

#[tokio::test(flavor = "multi_thread")]
async fn unwinds_that_never_went_out_do_not_end_a_stopped_stations_unwinding() {
    // Five seconds into a slot, and every round an hour on: no round lands
    // in a valuation window, so nothing is valued.
    let node = node(1_700_000_005);
    a_stopped_delayed_station(&node);
    // The same closing order from the same figures every time; nothing to
    // sweep; money may be moved through the driver at once.
    answers(
        &node,
        DRIVER,
        IDriver::closeCall {
            station: STATION,
            fraction: U256::from(WAD),
            bandBps: 100,
        },
        IDriver::closeReturn {
            calls: vec![a_call(0xc1, 0)],
            blocked: false,
        },
    );
    answers(
        &node,
        DRIVER,
        IDriver::sweepCall {
            station: STATION,
            amount: U256::MAX,
        },
        vec![],
    );
    answers(
        &node,
        DRIVER,
        IDelayedDriver::movableAtCall { station: STATION },
        U256::ZERO,
    );
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("rounds", 0);
        locked.vars.insert("turned_away", 0);
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 3_600;
                *node.vars.get_mut("rounds").unwrap() += 1;
                return Some(Ok(IVault::stateCall::abi_encode_returns(
                    &VaultState::Killed,
                )));
            }
            None
        }));
        locked.on_send = Some(Arc::new(|node, to, data| {
            if to == STATION
                && data[..4] == IStation::unwindCall::SELECTOR
                && node.vars["rounds"] <= UNFUNDED_ROUNDS
            {
                *node.vars.get_mut("turned_away").unwrap() += 1;
                return Some(serde_json::json!({
                    "code": -32000,
                    "message": "insufficient funds for gas * price + value",
                }));
            }
            None
        }));
    }
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("unwinds-never-went-out", &rpc, "http://127.0.0.1:1");

    // A day of rounds after the account is funded again: the longest the
    // keeper waits between two unwinds that change nothing is five minutes.
    let hours = UNFUNDED_ROUNDS + 24;
    assert!(
        wait_until(Duration::from_secs(300), || {
            let node = node.lock().unwrap();
            node.vars["rounds"] >= hours
                || node
                    .sent
                    .iter()
                    .any(|(to, data)| *to == STATION && data[..4] == IStation::unwindCall::SELECTOR)
        })
        .await,
        "the keeper did not run {hours} rounds: {}",
        keeper.logged()
    );
    let (rounds, turned_away) = {
        let node = node.lock().unwrap();
        (node.vars["rounds"], node.vars["turned_away"])
    };
    let unwinds = sent::<IStation::unwindCall>(&node, STATION);
    assert!(
        unwinds >= 1,
        "the node turned {turned_away} unwinds away for want of gas money in the first \
         {UNFUNDED_ROUNDS} rounds; in the {} rounds an hour apart since, with positions still to \
         close at the venue, the keeper sent none",
        rounds.saturating_sub(UNFUNDED_ROUNDS)
    );
}
