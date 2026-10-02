// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault, Request, ValuationStatus} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// Between a valuation being priced and its requests being settled, neither
/// the redeem fallback nor a claim's close takes the cash the priced
/// withdrawals are to be paid from: both wait for the settlement.

/// A redeem that nothing priced for a day takes the fallback right after a
/// valuation was priced, before its withdrawals were settled.
contract RedeemFallbackWaitsOnTwoChainsTest is TwoChainVault {
    function test_the_fallback_waits_for_the_withdrawals_a_closed_valuation_priced() public {
        depositTwo(alice, 250_000e6);
        uint256 bobShares = depositTwo(bob, 50_000e6);
        // 200k of the 300k goes to the other chain; 100k stays as cash.
        vm.startPrank(executor);
        station.pull(200_000e6);
        station.bridge("local-bridge", OTHER, 200_000e6, "");
        vm.stopPrank();
        assertEq(vault.cash(), 100_000e6 + DEAD_ASSETS);

        // Bob asks for all of his (worth 50k): priced at the next slot.
        vm.prank(bob);
        uint256 bobRequest = vault.requestRedeem(bobShares, bob);
        uint64 time = open();
        assertLe(vault.requestAt(bobRequest).earliest, time);

        // Inside the window alice asks for all of hers: the next slot.
        vm.warp(time + 3);
        uint256 aliceShares = vault.balanceOf(alice);
        vm.prank(alice);
        uint256 aliceRequest = vault.requestRedeem(aliceShares, alice);
        Request memory r = vault.requestAt(aliceRequest);
        assertGt(r.earliest, time);

        // The other chain posts, but nobody closes the valuation for a day.
        post(time, postOf(200_000e6, 200_000e6), EXECUTOR_KEY);
        vm.warp(r.earliest + 24 hours + 1);

        // Now it is closed: priced, with bob's withdrawal waiting on it, and
        // the cash (100k) covers him.
        vault.closeValuation(time);
        assertTrue(latest().status == ValuationStatus.Priced);
        assertFalse(vault.requestAt(bobRequest).done);
        assertGe(vault.cash(), 50_000e6);

        // Before anyone settles, alice's fallback waits.
        vm.prank(alice);
        vm.expectRevert(IVault.NotYet.selector);
        vault.takeProRata(aliceRequest);

        vault.settle(type(uint256).max);
        assertTrue(vault.requestAt(bobRequest).done);
        assertEq(vault.claimCount(), 0, "bob's withdrawal, priced and covered by cash, became a claim");
        assertGt(usd.balanceOf(bob), 0, "bob was not paid at the price he was given");

        vm.prank(alice);
        vault.takeProRata(aliceRequest);
        assertGt(usd.balanceOf(alice), 0);
    }
}

/// A claim's close waits for a single-chain valuation's settlement.
contract ClaimCloseWaitsForPricedWithdrawalsTest is ClaimBase {
    function test_a_claims_close_waits_for_the_withdrawals_a_valuation_priced() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        depositCarol(100_000e6);
        // 250k of the 300k goes to the venue, most of it into spot; 50k of
        // cash stays home.
        deploySpot(250_000e6, 250_000e6, 4e18);
        assertEq(vault.cash(), 50_000e6 + DEAD_ASSETS);

        // Alice's withdrawal (worth 100k) can't be paid from 50k: a claim.
        uint256 claimId = claimFor(alice, aliceShares);

        // Bob asks for 40% of his (worth 40k, which the cash covers), and a
        // valuation prices it, but nothing is settled yet.
        vm.prank(bob);
        uint256 bobRequest = vault.requestRedeem(bobShares * 2 / 5, bob);
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        assertFalse(vault.requestAt(bobRequest).done);
        assertGe(vault.cash(), 40_000e6);

        // Alice's claim can't be closed before bob is settled.
        vm.expectRevert(IVault.NotYet.selector);
        vault.settleClaim(claimId);

        vault.settle(type(uint256).max);
        assertTrue(vault.requestAt(bobRequest).done);
        assertEq(vault.claimCount(), 1, "bob's withdrawal, priced and covered by cash, became a claim");

        vault.settleClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Paid);
    }
}
