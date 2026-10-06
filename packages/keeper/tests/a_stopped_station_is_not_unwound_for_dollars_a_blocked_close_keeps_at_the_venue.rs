//! A halted vault past its grace, its stopped station trading through one
//! driver whose venue acts after the transaction. A market the station holds
//! is outside the stopped station's band, so the driver's full close is
//! blocked; the account also has dollars free at the venue, and what it
//! could send home changes with every tick of the market.
//!
//! The station's full unwind does nothing through a driver whose close is
//! blocked: it places no order and sweeps nothing, since the account's free
//! dollars stay with its positions until the band reaches them. With nothing
//! idle at the station the unwind then does nothing at all, so the keeper
//! must not pay for one every slot for as long as the market stays outside
//! the band.

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

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_station_is_not_unwound_for_dollars_a_blocked_close_keeps_at_the_venue() {
    // Five seconds into a slot, and every round an hour on: no round lands
    // in a valuation window, so nothing is valued.
    let node = node(1_700_000_005);
    a_stopped_delayed_station(&node);
    // The close is blocked: a market outside the band.
    answers(
        &node,
        DRIVER,
        IDriver::closeCall {
            station: STATION,
            fraction: U256::from(WAD),
            bandBps: 100,
        },
        IDriver::closeReturn {
            calls: vec![],
            blocked: true,
        },
    );
    // Money may be moved through the driver at once.
    answers(
        &node,
        DRIVER,
        IDelayedDriver::movableAtCall { station: STATION },
        U256::ZERO,
    );
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("rounds", 0);
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 3_600;
                *node.vars.get_mut("rounds").unwrap() += 1;
                return Some(Ok(IVault::stateCall::abi_encode_returns(
                    &VaultState::Killed,
                )));
            }
            // Free dollars at the venue, a different amount every round.
            if to == DRIVER && data[..4] == IDriver::sweepCall::SELECTOR {
                return Some(Ok(IDriver::sweepCall::abi_encode_returns(&vec![a_call(
                    0x5e,
                    node.vars["rounds"],
                )])));
            }
            None
        }));
    }
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("unwind-blocked-close", &rpc, "http://127.0.0.1:1");

    let hours = 12;
    assert!(
        wait_until(Duration::from_secs(240), || {
            node.lock().unwrap().vars["rounds"] >= hours
        })
        .await,
        "the keeper did not run {hours} rounds: {}",
        keeper.logged()
    );
    let rounds = node.lock().unwrap().vars["rounds"];
    let unwinds = sent::<IStation::unwindCall>(&node, STATION);
    assert!(
        unwinds <= 2,
        "{unwinds} unwinds paid for in {rounds} rounds while the driver's close was blocked: the \
         station places no order and sweeps nothing through a blocked driver, and nothing was \
         idle to send home"
    );
}
