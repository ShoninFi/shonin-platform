// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LighterBoard} from "../../src/drivers/lighter/LighterBoard.sol";
import {LighterDriver} from "../../src/drivers/lighter/LighterDriver.sol";
import {IZkLighter} from "../../src/drivers/lighter/ZkLighter.sol";
import {Call, IDriver} from "../../src/interfaces/IDriver.sol";
import {StationDouble} from "../robinhood/StationDouble.sol";
import {Test, Vm} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// The Lighter driver's calls against Lighter's real exchange contract, on a
/// fork of Robinhood Chain mainnet: a contract that deposits owns a new
/// account, and the exchange queues its key change, its closing orders and
/// its withdrawals as priority requests. What the exchange then does with
/// them happens off-chain and can't be seen on a fork.
///
/// Runs only with `ROBINHOOD_FORK_URL` set; skipped otherwise.
contract LighterForkTest is Test {
    address internal constant ZK_LIGHTER = 0x94bAB9693Ba2f6358507eFfcbd372b0660AFfF9d;
    address internal constant USDG = 0x5fc5360D0400a0Fd4f2af552ADD042D716F1d168;
    bytes internal constant PUBKEY =
        hex"0a000000000000000b000000000000000c000000000000000d000000000000000e00000000000000";
    /// What the exchange emits for each priority request it queues.
    bytes32 internal constant NEW_PRIORITY_REQUEST =
        keccak256("NewPriorityRequest(address,uint64,uint8,bytes,uint64)");

    bool internal forked;
    LighterBoard internal board;
    LighterDriver internal lighter;
    StationDouble internal station;
    address internal executor = makeAddr("executor");

    function setUp() public {
        string memory url = vm.envOr("ROBINHOOD_FORK_URL", string(""));
        if (bytes(url).length == 0) return;
        vm.createSelectFork(url);
        forked = true;
        board = new LighterBoard();
        LighterDriver.Market[] memory markets = new LighterDriver.Market[](3);
        // ETH, SPY and QQQ as the exchange lists them on Robinhood Chain.
        markets[0] = LighterDriver.Market({index: 0, sizeDecimals: 4, priceDecimals: 2});
        markets[1] = LighterDriver.Market({index: 26, sizeDecimals: 4, priceDecimals: 2});
        markets[2] = LighterDriver.Market({index: 25, sizeDecimals: 4, priceDecimals: 2});
        lighter = new LighterDriver(IZkLighter(ZK_LIGHTER), IERC20(USDG), board, 1 hours, 6, markets);
        station = new StationDouble(USDG, 100);
        deal(USDG, address(station), 100e6);
        vm.prank(executor);
        board.setPubKey(PUBKEY);
    }

    modifier onFork() {
        if (!forked) {
            vm.skip(true);
            return;
        }
        _;
    }

    function _requests(Vm.Log[] memory logs) internal pure returns (uint256 count) {
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter == ZK_LIGHTER && logs[i].topics[0] == NEW_PRIORITY_REQUEST) count++;
        }
    }

    function test_a_contracts_first_deposit_makes_it_an_account_owner_and_its_key_change_is_queued()
        public
        onFork
    {
        (, bool before) = lighter.masterOf(address(station));
        assertFalse(before);
        assertEq(lighter.approveKey(address(station), executor, 0).length, 0, "no account yet: nothing to do");
        vm.recordLogs();
        station.deposit(IDriver(address(lighter)), 5e6);
        station.approveKey(IDriver(address(lighter)), executor);
        (uint48 account, bool exists) = lighter.masterOf(address(station));
        assertTrue(exists, "the deposit made the station an account");
        assertGt(account, 38_136, "a new account, after the spike's");
        assertEq(IERC20(USDG).balanceOf(address(station)), 95e6);
        assertEq(_requests(vm.getRecordedLogs()), 2, "the deposit and the key change are queued");
    }

    function test_closing_withdrawing_and_revoking_are_priority_requests_the_exchange_accepts_from_the_owner()
        public
        onFork
    {
        station.deposit(IDriver(address(lighter)), 5e6);
        station.approveKey(IDriver(address(lighter)), executor);
        vm.recordLogs();
        (Call[] memory calls, bool blocked) = lighter.close(address(station), 1e18, 10_000);
        assertFalse(blocked);
        station.run(IDriver(address(lighter)), calls);
        assertEq(_requests(vm.getRecordedLogs()), 6, "both sides of three markets, whole, at any price");

        station.withdraw(IDriver(address(lighter)), 2e6);
        assertEq(_requests(vm.getRecordedLogs()), 1, "the withdrawal");

        station.revokeKey(IDriver(address(lighter)), executor);
        assertEq(_requests(vm.getRecordedLogs()), 2, "the key replaced and the orders cancelled");
    }

    function test_a_stranger_cannot_queue_requests_for_the_stations_account() public onFork {
        station.deposit(IDriver(address(lighter)), 5e6);
        station.approveKey(IDriver(address(lighter)), executor);
        (uint48 account,) = lighter.masterOf(address(station));
        // The exchange puts the caller's own master account in every request,
        // so a stranger's request names the stranger, who has none.
        vm.prank(makeAddr("stranger"));
        vm.expectRevert();
        IZkLighter(ZK_LIGHTER).withdraw(account, 3, 0, 1e6);
    }
}
