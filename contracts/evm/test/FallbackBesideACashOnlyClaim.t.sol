// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, Request, Valuation} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// A claim queued on the cash alone closed nothing, so its shares still own
/// their fraction of everything: a leaver taking the fallback beside it is
/// paid their share of all the cash, its set-aside part included.
contract FallbackBesideACashOnlyClaimTest is TwoChainVault {
    function test_a_fallback_beside_a_claim_queued_on_cash_pays_its_fraction() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        uint256 bobShares = depositTwo(bob, 100_000e6);
        depositTwo(author, 100_000e6);

        // Most of the money goes to the home station; 50k stays as cash, so
        // alice's withdrawal can't be paid and becomes a claim.
        vm.prank(executor);
        station.pull(250_000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        valueWith(0, 0);
        assertEq(vault.claimCount(), 1);
        assertTrue(vault.claimAt(1).state == ClaimState.Pending);

        // The executor brings 200k back: the cash now covers the claim, so
        // `settleClaim` queues it on the cash alone and closes nothing.
        vm.prank(executor);
        station.push(200_000e6);
        valueWith(0, 0);
        uint256 cash = vault.cash();
        uint256 supply = vault.totalSupply();
        uint256 reserved = expectedPayout(aliceShares);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);
        assertEq(vault.cash(), cash, "a claim queued on the cash alone moved no money");

        // The other chain goes quiet for a day: nothing can price bob, and
        // the fallback opens for him.
        vm.prank(bob);
        uint256 request = vault.requestRedeem(bobShares, bob);
        Request memory r = vault.requestAt(request);
        vm.warp(r.earliest + 24 hours + 1);
        uint256 before = usd.balanceOf(bob);
        vm.prank(bob);
        vault.takeProRata(request);
        uint256 paid = usd.balanceOf(bob) - before;

        // Alice's shares still own their fraction of everything, so bob's
        // share of the cash is his shares over every share, of all the cash.
        uint256 fair = (cash * bobShares) / supply;
        emit log_named_uint("cash", cash);
        emit log_named_uint("reserved for the cash-only claim", reserved);
        emit log_named_uint("paid to bob", paid);
        emit log_named_uint("bob's share of the cash", fair);

        // With his units' worth of the station, bob leaves with his third.
        (uint256 held,) = vault.unitsOf(bob);
        (, uint256 stationExit) = station.value();
        uint256 bobTotal = paid + (stationExit * held) / 1e18;
        uint256 bobFair = ((cash + stationExit) * bobShares) / supply;
        emit log_named_uint("bob's total with his units", bobTotal);
        emit log_named_uint("bob's third of the vault", bobFair);
        assertApproxEqRel(bobTotal, bobFair, 0.001e18, "bob left with less than his fraction");
        assertEq(paid, fair, "the fallback left the cash-only claim's set-aside cash out of what it shares");
    }

    /// Mirror of `Valuations.exitOf(...).payout` at the latest valuation.
    function expectedPayout(uint256 shares) internal view returns (uint256 payout) {
        Valuation memory v = latest();
        uint256 slice = v.feeExit == 0 ? 0 : ceilDiv(shares * v.feeExit, v.grossExit);
        uint256 gross = ((shares - slice) * v.grossExit) / v.supply;
        payout = gross - ceilDiv(gross * 10, 10_000);
    }

    function ceilDiv(uint256 a, uint256 b) internal pure returns (uint256) {
        return (a + b - 1) / b;
    }
}
