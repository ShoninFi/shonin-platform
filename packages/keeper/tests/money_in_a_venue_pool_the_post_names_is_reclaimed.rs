//! An open vault whose station trades through the Lighter driver. The
//! station's account holds shares in one of the exchange's public pools
//! (which only its trading key could have bought): the newest post names
//! them, worth five thousand dollars, apart from the account's equity. The
//! station counts money parked like that only once `reclaim` has listed the
//! pool, and brings it back only through `reclaim`, which anyone may call
//! and which the driver prepares (it burns the shares).
//!
//! Nothing else on the chain or off it calls `reclaim`: the keeper is who
//! watches for parked money. Here everything it needs is on the chain: the
//! driver names its board, the board holds the post, the driver's `reclaim`
//! for that pool says five thousand dollars and has the call ready, and the
//! station's `reclaim` would go through. The keeper must send it.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, Bytes, U256, aliases::U48};
use alloy::sol_types::SolCall;
use chain::bindings::driver::{Call, ExitPath, IDelayedDriver, IDriver};
use chain::bindings::station::{IStation, IStationVault, Stage};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    STATION, VAULT, a_spec, a_tank, answers, name, node, one_vault, sent, serve_node, start_keeper,
    timers, wait_until,
};

alloy::sol! {
    struct LighterPosition {
        uint48 account;
        uint16 market;
        int64 size;
    }
    struct LighterPrice {
        uint16 market;
        uint32 mark;
        uint32 index;
        uint32 bid;
        uint32 ask;
    }
    struct LighterShares {
        uint48 account;
        uint48 pool;
        uint64 shares;
        uint256 value;
    }
    struct LighterReport {
        uint64 asOf;
        uint48[] accounts;
        uint256 equity;
        uint256 pending;
        uint256 inFlight;
        uint256 arrived;
        uint256 sent;
        uint256[] available;
        LighterPosition[] positions;
        LighterPrice[] prices;
        LighterShares[] pools;
    }
    interface ILighterDriver {
        function exchange() external view returns (address);
        function DOLLAR_ASSET() external view returns (uint16);
        function board() external view returns (address);
    }
    interface ILighterBoard {
        function reportOf(address station) external view returns (LighterReport memory report, bool exists);
    }
    interface ILighterExchange {
        function getPendingBalance(address owner, uint16 assetIndex) external view returns (uint128);
    }
}

const DRIVER: Address = Address::repeat_byte(0x77);
const EXCHANGE: Address = Address::repeat_byte(0x88);
const BOARD: Address = Address::repeat_byte(0x99);
const POOL: u64 = 281_474_976_710_654;

/// A pool's index as the station and the driver name it: as an address.
fn pool_address() -> Address {
    let mut bytes = [0u8; 20];
    bytes[12..].copy_from_slice(&POOL.to_be_bytes());
    Address::from(bytes)
}

#[tokio::test(flavor = "multi_thread")]
async fn money_in_a_venue_pool_the_post_names_is_reclaimed() {
    // Five seconds into a slot, and every round a slot on: no round lands in
    // a valuation window, so nothing is valued.
    let start = 1_700_000_005u64;
    let node = node(start);
    one_vault(&node);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(&node, VAULT, IVault::tankCall {}, a_tank(50_000_000));
    answers(&node, VAULT, IStationVault::haltedAtCall {}, 0);
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
    answers(&node, STATION, IStation::stoppedCall {}, false);
    answers(&node, STATION, IStation::outsideBandCall {}, false);
    // Not listed yet, so not counted in the station's value.
    answers(
        &node,
        STATION,
        IStation::heldInCall { driver: lighter },
        vec![],
    );
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
            provedBy: u64::MAX,
            forVault: false,
        },
    );
    // The station's `reclaim` of that pool would go through.
    node.lock().unwrap().exact.insert(
        (
            STATION,
            IStation::reclaimCall {
                driver: lighter,
                venueVault: pool_address(),
            }
            .abi_encode(),
        ),
        Ok(Vec::new()),
    );

    answers(&node, DRIVER, IDriver::exitPathCall {}, ExitPath::Delayed);
    answers(&node, DRIVER, IDriver::nameCall {}, name("lighter"));
    answers(&node, DRIVER, ILighterDriver::exchangeCall {}, EXCHANGE);
    answers(&node, DRIVER, ILighterDriver::DOLLAR_ASSETCall {}, 3);
    answers(&node, DRIVER, ILighterDriver::boardCall {}, BOARD);
    answers(
        &node,
        EXCHANGE,
        ILighterExchange::getPendingBalanceCall {
            owner: STATION,
            assetIndex: 3,
        },
        0,
    );
    // The newest post: twenty thousand dollars of equity, and beside it the
    // pool's shares, worth five thousand.
    answers(
        &node,
        BOARD,
        ILighterBoard::reportOfCall { station: STATION },
        ILighterBoard::reportOfReturn {
            report: LighterReport {
                asOf: start - 30,
                accounts: vec![U48::from(7u64)],
                equity: U256::from(20_000_000_000u64),
                pending: U256::ZERO,
                inFlight: U256::ZERO,
                arrived: U256::ZERO,
                sent: U256::from(25_000_000_000u64),
                available: vec![U256::from(20_000_000_000u64)],
                positions: vec![],
                prices: vec![],
                pools: vec![LighterShares {
                    account: U48::from(7u64),
                    pool: U48::from(POOL),
                    shares: 5_000,
                    value: U256::from(5_000_000_000u64),
                }],
            },
            exists: true,
        },
    );
    // The driver's own answer for that pool: what it holds, and the call
    // that burns the shares.
    node.lock().unwrap().exact.insert(
        (
            DRIVER,
            IDelayedDriver::reclaimCall {
                station: STATION,
                held: pool_address(),
            }
            .abi_encode(),
        ),
        Ok(IDelayedDriver::reclaimCall::abi_encode_returns(
            &IDelayedDriver::reclaimReturn {
                calls: vec![Call {
                    target: EXCHANGE,
                    value: U256::ZERO,
                    data: Bytes::from_static(&[0xb0, 0x0b, 0x5a, 0xfe]),
                }],
                equity: U256::from(5_000_000_000u64),
            },
        )),
    );
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("rounds", 0);
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 10;
                *node.vars.get_mut("rounds").unwrap() += 1;
                return Some(Ok(IVault::stateCall::abi_encode_returns(&VaultState::Open)));
            }
            None
        }));
    }
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("venue-pool-reclaimed", &rpc, "http://127.0.0.1:1");

    // The keeper looks for parked money once a minute at most, and at once
    // the first time: ten rounds are more than it needs.
    assert!(
        wait_until(Duration::from_secs(120), || {
            node.lock().unwrap().vars["rounds"] >= 10
        })
        .await,
        "the keeper did not run ten rounds: {}",
        keeper.logged()
    );
    let rounds = node.lock().unwrap().vars["rounds"];
    let reclaims = sent::<IStation::reclaimCall>(&node, STATION);
    assert!(
        reclaims >= 1,
        "five thousand dollars in a pool the newest post names, the driver's reclaim ready and \
         the station's would go through, and in {rounds} rounds the keeper never sent it"
    );
}
