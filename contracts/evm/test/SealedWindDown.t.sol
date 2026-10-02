// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// When every holder of the root key is lost, sealed agents can't run again,
/// so the admin key winds the vault down: it stops, its stations are unwound
/// and closed, and every holder is paid, as at a retirement's end.
contract SealedWindDownTest is ClaimBase {
    function test_only_the_admin_winds_a_sealed_vault_down() public {
        vm.prank(author);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.windDownSealed();
        vm.prank(alice);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.windDownSealed();
    }

    function test_a_sealed_wind_down_unwinds_closes_and_pays_everyone() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        deploySpot(90_000e6, 60_000e6, 0.5e18);

        vm.expectEmit(address(vault));
        emit IVault.Halted("sealed", uint64(block.timestamp));
        vm.prank(admin);
        vault.windDownSealed();
        assertTrue(vault.state() == VaultState.Killed);

        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);
        assertEq(spotSize(), 0, "positions are still open");
        value(vault);
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);

        vm.prank(alice);
        uint256 paid = vault.redeemClosed(aliceShares, alice);
        assertApproxEqRel(paid, 100_000e6, 0.01e18, "the holder was not paid out");
    }
}
