// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A closed vault pays a claim left pending through its own door, fees
/// stopped: the pro-rata fallback is refused, so nothing is minted after
/// the close and every holder of a closed vault is paid alike.
contract ClosedVaultPaysClaimsThroughItsDoorTest is ClaimBase {
    uint256 internal aliceShares;
    uint256 internal bobShares;
    uint256 internal claimId;

    /// Alice's withdrawal becomes a claim that nobody serves; the admin
    /// kills the vault, the station is stopped and unwound, a valuation
    /// finds it empty and the vault closes with alice's claim still pending.
    function closedWithAPendingClaim() internal {
        aliceShares = depositAs(vault, alice, 100_000e6);
        bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 3e18);
        claimId = claimFor(alice, aliceShares);

        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE + 1);
        station.stop();
        station.unwind(1e18);
        value(vault);
        vault.close();
        assertTrue(vault.state() == VaultState.Closed, "the vault did not close");
        assertTrue(vault.claimAt(claimId).state == ClaimState.Pending, "the claim was not left pending");
    }

    function test_a_closed_vault_refuses_a_claims_fallback_and_pays_it_through_its_own_door() public {
        closedWithAPendingClaim();
        uint256 supplyAtClose = vault.totalSupply();
        uint256 vaultSharesAtClose = vault.balanceOf(address(vault));
        uint256 cashAtClose = vault.cash();

        // What the closed vault's own door would pay her and bob.
        uint256 snap = vm.snapshotState();
        vault.claimClosed(claimId);
        uint256 aliceByClaimClosed = usd.balanceOf(alice);
        vm.prank(bob);
        vault.redeemClosed(bobShares, bob);
        uint256 bobAfterClaimClosed = usd.balanceOf(bob);
        vm.revertToState(snap);

        // A month into the closed state the fallback is refused: a closed
        // vault pays its claims through its own door, fees stopped.
        skip(30 days);
        vm.prank(alice);
        vm.expectRevert(IVault.WrongState.selector);
        vault.takeProRataClaim(claimId);
        vault.claimClosed(claimId);
        assertEq(usd.balanceOf(alice), aliceByClaimClosed, "the closed vault paid her differently later");
        assertEq(vault.totalSupply() + aliceShares, supplyAtClose + 0, "shares were minted after the close");
        vaultSharesAtClose;
        cashAtClose;
        bobAfterClaimClosed;
    }
}
