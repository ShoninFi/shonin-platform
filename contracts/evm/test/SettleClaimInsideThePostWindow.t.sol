// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, Valuation, ValuationStatus} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// A claim's close made while a valuation waits for posts moves money from
/// the home station into cash after the station was valued: the snapshot's
/// total is unchanged, the supply can't move, and nobody else's price moves.
contract SettleClaimInsideThePostWindowTest is TwoChainVault {
    function test_a_close_inside_the_post_window_moves_nobody_elses_price() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        uint256 bobShares = depositTwo(bob, 100_000e6);
        vm.prank(executor);
        station.pull(190_000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        valueWith(0, 0);
        assertTrue(vault.claimAt(1).state == ClaimState.Pending);
        uint256 priceBefore = vault.sharePrice();

        // A valuation opens; inside its window the claim's fraction is
        // closed at the home station and comes home.
        uint64 time = open();
        uint256 cashAtOpen = vault.cash();
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);
        assertGt(vault.cash(), cashAtOpen, "the close brought nothing home");
        post(time, postOf(0, 0), EXECUTOR_KEY);
        closeAfterDeadline(time);
        Valuation memory v = latest();
        assertTrue(v.status == ValuationStatus.Priced);
        // The snapshot still adds up to everything the vault owns.
        assertEq(v.gross, cashAtOpen + 190_000e6 + 1, "the snapshot lost or double-counted the close");
        assertApproxEqRel(vault.sharePrice(), priceBefore, 0.0001e18, "the price moved inside the window");

        // The claim is priced at the next valuation and alice is paid her
        // fraction; bob's price is where it was.
        valueWith(0, 0);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        assertApproxEqRel(usd.balanceOf(alice), 100_000e6, 0.01e18);
        assertApproxEqRel(vault.sharePrice(), priceBefore, 0.0001e18, "bob paid for alice's close");
        bobShares;
    }
}
