// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault, Valuation} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A claim the cash covers is served at a price, through the queue, like any
/// other exit: the pro-rata fallback is refused to it, and to every claim
/// while a priced valuation waits to settle.
contract ServableClaimKeepsThePricedPathTest is ClaimBase {
    /// Mirror of `Valuations.exitOf(...).payout` at the latest valuation.
    function pricedPayout(uint256 shares) internal view returns (uint256 payout) {
        Valuation memory v = latest();
        uint256 slice = v.feeExit == 0 ? 0 : ceilDiv(shares * v.feeExit, v.grossExit);
        uint256 gross = ((shares - slice) * v.grossExit) / v.supply;
        payout = gross - ceilDiv(gross * 10, 10_000);
    }

    function test_a_cash_covered_claim_a_day_old_is_refused_the_fallback_and_paid_at_a_price() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 0, 0);
        uint256 id = claimFor(alice, aliceShares);

        // Everything comes home: the cash covers her claim.
        vm.prank(executor);
        station.push(190_000e6);
        value(vault);
        skip(1 days);

        vm.prank(alice);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.takeProRataClaim(id);

        // Anyone queues it, and the next valuation pays it at its price.
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Queued);
        value(vault);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid);
        assertEq(usd.balanceOf(alice), pricedPayout(aliceShares), "the claim was not paid its price");
    }

    function test_no_claim_takes_the_fallback_between_a_valuation_and_its_settlement() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        depositCarol(100_000e6);
        deploySpot(285_000e6, 0, 0);
        uint256 id = claimFor(alice, aliceShares);
        // 125k of cash: enough for bob's whole withdrawal, not for alice's
        // claim and his together.
        vm.prank(executor);
        station.push(110_000e6);
        value(vault);
        skip(1 days);

        vm.prank(bob);
        uint256 bobRequest = vault.requestRedeem(bobShares, bob);
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        uint256 bobPriced = pricedPayout(bobShares);
        assertLe(bobPriced, vault.cash());

        vm.prank(alice);
        vm.expectRevert(IVault.NotYet.selector);
        vault.takeProRataClaim(id);

        vault.settle(type(uint256).max);
        assertTrue(vault.requestAt(bobRequest).done);
        assertEq(vault.claimCount(), 1, "a redeem priced to be paid from cash became a claim");
        assertEq(usd.balanceOf(bob), bobPriced, "bob was not paid the price his valuation fixed");
    }
}
