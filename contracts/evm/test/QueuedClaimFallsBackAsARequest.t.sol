// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A claim the cash covers is refused the pro-rata fallback, and queuing it
/// doesn't open one: a queued claim is a request, and falls back only when
/// nothing has priced it for the fallback delay from its own slot.
contract QueuedClaimFallsBackAsARequestTest is ClaimBase {
    function test_a_covered_claim_queued_does_not_take_the_fallback_at_once() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        // The strategy holds 180k idle at its station: alice's withdrawal
        // can't be paid from the vault's cash and becomes a claim.
        deploySpot(180_000e6, 0, 0);
        uint256 claimId = claimFor(alice, aliceShares);

        // The cash comes home; a day passes with every slot priced.
        vm.prank(executor);
        station.push(180_000e6);
        skip(1 days);
        value(vault);

        // The rule: the cash covers her, so no fallback.
        vm.prank(alice);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.takeProRataClaim(claimId);

        // Queue it, then ask again in the same block.
        vm.prank(alice);
        vault.settleClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Queued);
        vm.prank(alice);
        vm.expectRevert(IVault.NotYet.selector);
        vault.takeProRataClaim(claimId);

        // The next valuation pays it at its price.
        value(vault);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Paid);
    }
}
