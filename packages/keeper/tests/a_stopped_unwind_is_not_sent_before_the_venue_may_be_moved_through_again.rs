//! A halted vault past its grace, its stopped station trading through one
//! driver whose venue acts after the transaction. The driver spaces every
//! move of dollars through it, whoever makes it: before the time it names
//! (`movableAt`), the station's full unwind skips that driver altogether,
//! placing no order and asking for nothing, and with nothing idle at the
//! station it then does nothing at all.
//!
//! Here the driver names a time that never comes within the test, while its
//! closing orders read differently every round (their limits follow the
//! venue's price), so the venue never "reads the same" to the keeper. An
//! unwind sent before the driver may be moved through is paid for and does
//! nothing: the keeper must send none.

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

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_unwind_is_not_sent_before_the_venue_may_be_moved_through_again() {
    // Five seconds into a slot, and every round an hour on: no round lands
    // in a valuation window, so nothing is valued.
    let node = node(1_700_000_005);
    a_stopped_delayed_station(&node);
    answers(
        &node,
        DRIVER,
        IDriver::sweepCall {
            station: STATION,
            amount: U256::MAX,
        },
        vec![],
    );
    // No move through the driver before the year 2096.
    answers(
        &node,
        DRIVER,
        IDelayedDriver::movableAtCall { station: STATION },
        U256::from(4_000_000_000u64),
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
            // Positions left to close, inside the band; the orders' limits
            // differ every round.
            if to == DRIVER && data[..4] == IDriver::closeCall::SELECTOR {
                return Some(Ok(IDriver::closeCall::abi_encode_returns(
                    &IDriver::closeReturn {
                        calls: vec![a_call(0xc1, node.vars["rounds"])],
                        blocked: false,
                    },
                )));
            }
            None
        }));
    }
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("unwind-before-movable", &rpc, "http://127.0.0.1:1");

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
    assert_eq!(
        unwinds, 0,
        "{unwinds} unwinds paid for in {rounds} rounds, every one before the driver may be moved \
         through again, when the station skips the driver and does nothing"
    );
}
