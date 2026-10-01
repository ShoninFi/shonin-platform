// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault, Valuation, ValuationStatus} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// A claim can't be closed while a valuation waits for posts: that
/// valuation prices everyone else without the claims already closed, as
/// they stood when it was recorded. Once it is closed, the claim is served
/// and paid its fraction, and nobody else's price moves.
contract NoClaimClosesInThePostWindowTest is TwoChainVault {
    function test_a_claim_waits_for_the_post_window_and_moves_nobody_elses_price() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        uint256 bobShares = depositTwo(bob, 100_000e6);
        vm.prank(executor);
        station.pull(190_000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        valueWith(0, 0);
        assertTrue(vault.claimAt(1).state == ClaimState.Pending);
        uint256 priceBefore = vault.sharePrice();

        // A valuation opens: the claim can't be closed inside its window.
        uint64 time = open();
        vm.expectRevert(IVault.WrongState.selector);
        vault.settleClaim(1);
        post(time, postOf(0, 0), EXECUTOR_KEY);
        closeAfterDeadline(time);
        assertApproxEqRel(vault.sharePrice(), priceBefore, 0.0001e18, "the price moved");

        // Once it is closed, the claim is served.
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);

        // The claim is priced at the next valuation and alice is paid her
        // fraction; bob's price is where it was.
        valueWith(0, 0);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        assertApproxEqRel(usd.balanceOf(alice), 100_000e6, 0.01e18);
        assertApproxEqRel(vault.sharePrice(), priceBefore, 0.0001e18, "bob paid for alice's close");
        bobShares;
    }
}
