// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IStation} from "../../src/interfaces/IStation.sol";

import {HyperliquidVault} from "./HyperliquidVault.sol";
import {Vm} from "forge-std/Vm.sol";

/// A stopped station's full unwind through a driver whose venue acts after
/// the transaction (Hyperliquid's) takes none of the fixed per-call
/// allowance a venue that settles in the transaction is given, so it is held
/// to no floor of gas for it: sent with as much gas as one of HyperEVM's
/// small blocks holds, it goes through and does exactly what it does with
/// plenty.
contract AFullBandUnwindThroughADelayedDriverFitsASmallBlockTest is HyperliquidVault {
    /// The gas a HyperEVM small block holds.
    uint256 internal constant SMALL_BLOCK = 2_000_000;

    function _stoppedAtTheFullBand() internal {
        _fundAndTrade();
        _stockGas();
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        skip(14 * 7 days);
        assertEq(station.stoppedBand(), 10_000, "the band is not full yet");
        _whenMovable();
    }

    function _unwind(uint256 gas) internal returns (uint256 sentHome, bool blocked, bool failed) {
        vm.recordLogs();
        (sentHome, blocked) = station.unwind{gas: gas}(1e18);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter == address(station) && logs[i].topics[0] == IStation.DriverFailed.selector) {
                failed = true;
            }
        }
    }

    function test_the_hyperliquid_full_band_unwind_goes_through_within_a_small_block() public {
        _stoppedAtTheFullBand();
        uint256 snapshot = vm.snapshotState();
        (uint256 sentWithPlenty, bool blockedWithPlenty, bool failedWithPlenty) = _unwind(30_000_000);
        uint64 movedWithPlenty = station.movedAt(HL);
        uint64 unwoundWithPlenty = station.fullUnwindAt(HL);
        vm.revertToState(snapshot);

        (uint256 sent, bool blocked, bool failed) = _unwind(SMALL_BLOCK);
        assertFalse(failedWithPlenty, "the driver's part failed with plenty of gas");
        assertFalse(failed, "the driver's part failed within a small block");
        assertEq(sent, sentWithPlenty, "a small block's unwind brought home other than with plenty");
        assertEq(blocked, blockedWithPlenty, "a small block's unwind ended otherwise than with plenty");
        assertEq(station.movedAt(HL), movedWithPlenty, "a small block's unwind moved otherwise");
        assertEq(
            station.fullUnwindAt(HL), unwoundWithPlenty, "a small block's unwind was not the full unwind"
        );
        assertGt(station.fullUnwindAt(HL), 0, "the full unwind was not placed");
    }
}
