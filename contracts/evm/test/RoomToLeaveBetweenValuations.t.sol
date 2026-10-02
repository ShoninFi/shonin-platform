// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, StationPost} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// On a vault that waits for other chains' posts, a valuation opens a full
/// slot after the last one closed at the soonest: however eagerly the keeper
/// values, every cycle leaves room for a leaver to take the fallback.
contract RoomToLeaveBetweenValuationsTest is TwoChainVault {
    /// The executor's honest post: a market it holds is outside the band.
    function postWide(uint64 time) internal {
        StationPost memory p = postOf(0, 0);
        p.outsideBand = true;
        post(time, p, EXECUTOR_KEY);
    }

    /// The keeper closes the open valuation (void) at the first second it
    /// may, then opens the next as soon as it is allowed to.
    function cycle(uint64 time) internal returns (uint64 next) {
        vm.warp(time + DEADLINE + 1);
        vault.closeValuation(time);
        // Not in the same block, nor in the slot that follows.
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vm.prank(executor);
        vm.expectRevert(IVault.NotYet.selector);
        vault.recordValuation();
        next = open();
        postWide(next);
    }

    function test_an_eager_keeper_leaves_room_to_take_the_fallback() public {
        uint256 shares = depositTwo(alice, 1000e6);
        vm.prank(alice);
        uint256 id = vault.requestRedeem(shares, alice);

        // A day and more of void valuations, the keeper as eager as it can be.
        uint64 time = open();
        postWide(time);
        uint64 end = time + 25 hours;
        while (time < end) {
            time = cycle(time);
        }

        // Right after the keeper's close, before it may open the next, alice
        // takes the fallback.
        vm.warp(time + DEADLINE + 1);
        vault.closeValuation(time);
        vm.prank(alice);
        vault.takeProRata(id);
        assertTrue(vault.requestAt(id).done, "the fallback never opened");
    }
}
