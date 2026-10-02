// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Request} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A closed vault settles what is left through its own doors, so a request
/// settled after the close never makes it read as unsettled, and anyone can
/// still bring a stopped station's stragglers home.
contract ClosedVaultIsNeverUnsettledTest is ClaimBase {
    function test_a_straggler_settled_after_the_close_leaves_the_stopped_unwind_open() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(150_000e6, 100_000e6, 1e18);

        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);
        assertEq(spotSize(), 0);

        // A redeem filed just before the valuation that finds the station
        // empty: a straggler, settled after the close.
        uint256 t = vm.getBlockTimestamp();
        uint256 slot = t - (t % SLOT) + SLOT;
        vm.warp(slot - 4);
        vm.prank(alice);
        uint256 straggler = vault.requestRedeem(aliceShares / 2, alice);
        Request memory r = vault.requestAt(straggler);
        vm.warp(slot);
        vault.recordValuation();
        vault.settle(type(uint256).max);
        assertGt(r.earliest, vault.valuationAt(vault.valuationCount() - 1).time, "not a straggler");
        vault.close();
        assertFalse(vault.unsettled());

        vault.settleClosed(straggler);

        // A late dollar reaches the stopped station (a venue releasing
        // margin after the close, a refund, a donation).
        give(address(station), 5e6);

        // Sound: a closed vault owes nothing to a priced valuation, so anyone
        assertFalse(vault.unsettled(), "a closed vault reports an unsettled valuation");
        vm.prank(carol);
        station.unwindStopped(new bytes32[](0));
        assertEq(usd.balanceOf(address(station)), 0, "the late dollar is stuck at the station");
    }
}
