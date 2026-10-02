// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault, ValuationStatus, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A market outside the band voids every valuation, so a withdrawal is
/// never priced and never becomes a claim on its own. A day on, its owner
/// may turn it into a claim, booked from when it was due: a close is tried
/// for it, and a week from then the vault can be frozen.
contract WideMarketWithdrawalBecomesAClaimTest is ClaimBase {
    function test_a_withdrawal_a_wide_market_never_prices_becomes_a_claim_that_can_freeze_the_vault()
        public
    {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        // Nearly everything is at the venue in spot BTC.
        deploySpot(190_000e6, 190_000e6, 3e18);
        value(vault);

        // The spread goes to 20% around an unchanged mark and oracle, and
        // stays there. Every valuation from here is void.
        setPrice(SPOT, PRICE, 2000);
        vm.prank(alice);
        uint256 request = vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertTrue(latest().status == ValuationStatus.Void);
        assertFalse(vault.requestAt(request).done, "her redeem was priced at a void valuation");

        // Not before a day has passed, and only by her.
        vm.prank(alice);
        vm.expectRevert(IVault.NotYet.selector);
        vault.claimInstead(request);
        skip(1 days);
        value(vault);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.claimInstead(request);
        vm.prank(alice);
        uint256 id = vault.claimInstead(request);
        assertTrue(vault.requestAt(request).done);
        assertEq(vault.claimAt(id).shares, aliceShares);
        assertEq(vault.claimAt(id).bookedAt, vault.requestAt(request).earliest);
        // Neither the redeem's fallback nor a second claim from it.
        vm.prank(alice);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.takeProRata(request);

        // The close is tried and refused while the market is wide.
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Waiting);

        // A week from when she was due, the vault can be frozen.
        vm.expectRevert(IVault.NotYet.selector);
        vault.freeze(id);
        vm.warp(vault.requestAt(request).earliest + 7 days);
        vault.freeze(id);
        assertTrue(vault.state() == VaultState.Frozen);
    }
}
