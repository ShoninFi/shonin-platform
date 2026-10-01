// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, Valuation} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// The band rule judges each station's post on its own: two posts that are
/// each inside the exit band, but disagree with each other, don't make a
/// spread neither of them saw, so a leaver the cash covers is paid and a
/// queued claim isn't bounced.
contract PostsJudgedOneByOneTest is TwoChainVault {
    uint256 internal constant BPS = 10_000;

    function outsideBand(uint256 markValue, uint256 exitValue) internal view returns (bool) {
        uint256 band = vault.exitBandBps();
        uint256 floor = (markValue * (BPS - band) + BPS - 1) / BPS;
        return exitValue < floor;
    }

    function test_two_posts_each_inside_the_band_do_not_trap_a_leaver_the_cash_covers() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        depositTwo(bob, 100_000e6);
        // All 200k is cash at home; the other chain's station holds ~10k.
        assertGe(vault.cash(), 200_000e6);

        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);

        uint64 time = open();
        // Each post is inside the band on its own.
        assertFalse(outsideBand(10_000e6, 9950e6), "the executor's post is wide by itself");
        assertFalse(outsideBand(10_100e6, 10_050e6), "the unwinder's post is wide by itself");
        post(time, postOf(10_000e6, 9950e6), EXECUTOR_KEY);
        post(time, postOf(10_100e6, 10_050e6), UNWINDER_KEY);
        closeAfterDeadline(time);

        Valuation memory v = latest();
        // The combined figure the vault priced on: highest mark, lowest exit.
        assertEq(v.gross, v.cash + 10_100e6 + 1);
        assertEq(v.grossExit, v.cash + 9950e6 + 1);

        // Alice's shares are worth about 105k and the cash is 200k, so the
        // withdrawal should have been paid from cash at this valuation.
        assertEq(
            vault.claimCount(),
            0,
            "a withdrawal the cash covers was booked as a claim because two in-band posts disagreed"
        );
        assertGt(usd.balanceOf(alice), 0, "alice was not paid");
    }

    /// The same mechanism bounces a claim that was already queued on the cash.
    function test_a_queued_claim_is_not_bounced_by_two_in_band_posts() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        depositTwo(bob, 100_000e6);
        vm.prank(executor);
        station.pull(190_000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        valueWith(0, 0);
        assertEq(vault.claimCount(), 1);

        // The money comes home; the claim is queued on the cash.
        vm.prank(executor);
        station.push(190_000e6);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);

        uint64 time = open();
        post(time, postOf(10_000e6, 9950e6), EXECUTOR_KEY);
        post(time, postOf(10_100e6, 10_050e6), UNWINDER_KEY);
        closeAfterDeadline(time);

        assertTrue(
            vault.claimAt(1).state == ClaimState.Paid,
            "a queued claim the cash covers was bounced to Pending by two in-band posts"
        );
    }
}
