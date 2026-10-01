// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault, Valuation, ValuationStatus} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// A claim can't be served while a valuation waits for posts: paying it
/// burns shares, and nothing may change the supply the valuation will be
/// priced on. Once the valuation is closed, the claim is served and paid its
/// fraction, and nobody else's price moves.
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

        // Once it is closed, the claim is served and paid; bob's price is
        // where it was.
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        valueWith(0, 0);
        assertApproxEqRel(usd.balanceOf(alice), 100_000e6, 0.01e18);
        // It can only rise: the transaction fee she paid stays in the pool.
        assertGe(vault.sharePrice(), priceBefore, "bob paid for alice's close");
        bobShares;
    }
}
