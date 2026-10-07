// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// Once a close staged at the home station has ended, the vault tries no
/// claim until a valuation is priced, and says so: the keeper reads it rather
/// than remembering having seen a stage.
contract NeedsValuationFirstAfterAStagedCloseTest is HyperliquidVault {
    function test_needs_valuation_first_from_a_staged_close_ending_until_a_valuation_is_priced() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        value(vault);
        assertFalse(vault.needsValuationFirst(), "wanted before any close");

        // Alice and Bob both leave; the cash pays neither, so each is a claim.
        uint256 bobShares = vault.balanceOf(bob);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        value(vault);
        assertEq(vault.claimCount(), 2, "two claims");

        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Closing, "her close is staged");
        assertFalse(vault.needsValuationFirst(), "wanted while the close is staged");

        _settleStaged();
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "she is paid");
        assertTrue(vault.needsValuationFirst(), "not wanted once the close ended");
        vm.expectRevert(IVault.NotYet.selector);
        vault.settleClaim(2);

        // A priced valuation is what the vault waited for: Bob's claim is
        // tried again.
        value(vault);
        assertFalse(vault.needsValuationFirst());
        vault.settleClaim(2);
        assertGt(vault.claimAt(2).lastAttemptAt, 0, "his claim is tried");
    }
}
