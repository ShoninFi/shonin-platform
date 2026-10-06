//! A halted vault past its grace, no Closer, its stopped station trading
//! through one driver whose venue acts after the transaction. The driver
//! prepares closing orders from the last figures an attested machine posted
//! for the station, and nobody posts any more (the station's keys are gone),
//! so its full close names the same orders for good, whatever is left at the
//! venue: nothing on the chain can tell it the positions are closed.
//!
//! Every unwind the keeper sends places those orders again and brings
//! nothing home. What the venue holds, what the driver would sweep and what
//! the station would send home all read the same before and after each one.
//! An unwind that changes nothing is paid for and does nothing, so the keeper
//! must come to leave such a station alone for longer and longer: here the
//! chain moves an hour a round for a day, and the keeper must not have paid
//! for an unwind in every one of those hours.

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

alloy::sol! {
    interface IPendingDriver {
        function exchange() external view returns (address);
        function DOLLAR_ASSET() external view returns (uint16);
    }
    interface IPendingExchange {
        function getPendingBalance(address owner, uint16 assetIndex) external view returns (uint128);
    }
}

const DRIVER: Address = Address::repeat_byte(0x77);
const EXCHANGE: Address = Address::repeat_byte(0x88);

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_station_whose_venue_never_changes_is_not_unwound_for_good() {
    // Five seconds into a slot, and every round an hour on: no round lands
    // in a valuation window, so nothing is valued.
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

    let lighter = name("lighter");
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![lighter]));
    answers(
        &node,
        STATION,
        IStation::approvedKeyCall { driver: lighter },
        Address::ZERO,
    );
    answers(
        &node,
        STATION,
        IStation::pinnedDriverCall { driver: lighter },
        DRIVER,
    );
    answers(&node, STATION, IStation::stoppedCall {}, true);
    answers(&node, STATION, IStation::outsideBandCall {}, false);
    answers(&node, STATION, IStation::stoppedBandCall {}, 100);
    answers(
        &node,
        STATION,
        IStation::heldInCall { driver: lighter },
        vec![],
    );
    // No close is staged.
    answers(
        &node,
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
    // The last post left the station worth forty dollars at the venue; its
    // unwind brings nothing home and reports itself not done, since it
    // ordered something.
    answers(
        &node,
        STATION,
        IStation::valueCall {},
        IStation::valueReturn {
            mark: U256::from(40_000_000u64),
            exit: U256::from(40_000_000u64),
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
            blocked: true,
        },
    );

    answers(&node, DRIVER, IDriver::exitPathCall {}, ExitPath::Delayed);
    answers(&node, DRIVER, IPendingDriver::exchangeCall {}, EXCHANGE);
    answers(&node, DRIVER, IPendingDriver::DOLLAR_ASSETCall {}, 3);
    answers(
        &node,
        EXCHANGE,
        IPendingExchange::getPendingBalanceCall {
            owner: STATION,
            assetIndex: 3,
        },
        0,
    );
    // The same reduce-only order from the same stale figures, every time,
    // inside the band; nothing to sweep; the same exposure.
    answers(
        &node,
        DRIVER,
        IDriver::closeCall {
            station: STATION,
            fraction: U256::from(WAD),
            bandBps: 100,
        },
        IDriver::closeReturn {
            calls: vec![Call {
                target: EXCHANGE,
                value: U256::ZERO,
                data: Bytes::from_static(&[0xc1, 0x05, 0xe0, 0x00]),
            }],
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
        IDelayedDriver::exposureCall { station: STATION },
        vec![
            I256::try_from(5_000i64).unwrap(),
            I256::try_from(1_698_999_000i64).unwrap(),
            I256::try_from(2_000_000i64).unwrap(),
        ],
    );
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("rounds", 0);
        // An hour a round.
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
    }
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("stopped-venue-never-changes", &rpc, "http://127.0.0.1:1");

    let hours = 24;
    assert!(
        wait_until(Duration::from_secs(240), || {
            node.lock().unwrap().vars["rounds"] >= hours
        })
        .await,
        "the keeper did not run {hours} rounds: {}",
        keeper.logged()
    );
    let rounds = node.lock().unwrap().vars["rounds"];
    let unwinds = sent::<IStation::unwindCall>(&node, STATION) as u64;
    assert!(
        unwinds > 0,
        "the stopped station was never unwound: {}",
        keeper.logged()
    );
    // Sixteen of twenty-four leaves room for a wait that doubles from a slot
    // to several hours; an unwind in every hour is one that never backs off
    // past an hour, for as long as the venue reads the same.
    assert!(
        unwinds <= 16,
        "{unwinds} unwinds paid for in {rounds} rounds an hour apart, each placing the same \
         orders at a venue that read the same before and after it"
    );
}
