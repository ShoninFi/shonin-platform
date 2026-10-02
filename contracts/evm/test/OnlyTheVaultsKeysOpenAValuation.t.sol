// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault, Request} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// A valuation that waits for other chains' posts is opened only by a key
/// blessed for the vault: a stranger can't keep one open at every block, so
/// when the executor goes silent no valuation is open and the pro-rata
/// fallback opens a day later, for a redeem and for a claim alike.
contract OnlyTheVaultsKeysOpenAValuationTest is TwoChainVault {
    function slot() internal {
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
    }

    function test_a_stranger_cannot_open_a_valuation_and_the_fallback_opens() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        depositTwo(bob, 100_000e6);
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        Request memory r = vault.requestAt(id);

        // The executor goes silent; a stranger tries to open valuations.
        slot();
        vm.prank(makeAddr("stranger"));
        vm.expectRevert(IVault.NotBlessed.selector);
        vault.recordValuation();

        // A day on, with nothing open and nothing priced, alice leaves.
        vm.warp(r.earliest + 24 hours + 1);
        vm.prank(alice);
        vault.takeProRata(id);
        assertGt(usd.balanceOf(alice), 0, "the fallback did not open");
    }

    function test_a_claim_reaches_the_fallback_whatever_a_stranger_does() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        depositTwo(bob, 100_000e6);
        vm.prank(executor);
        station.pull(190_000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        valueWith(0, 0);
        assertTrue(vault.claimAt(1).state == ClaimState.Pending);

        slot();
        vm.prank(makeAddr("stranger"));
        vm.expectRevert(IVault.NotBlessed.selector);
        vault.recordValuation();

        skip(1 days);
        vm.prank(alice);
        vault.takeProRataClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Fallback);
        assertGt(usd.balanceOf(alice), 0);
    }
}
