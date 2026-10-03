// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, Request, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";
import {FaultingDriver} from "./FaultingDriverEscape.t.sol";

/// Units nobody could serve for a week freeze the vault, as a claim unpaid
/// for a week does: a station that can't bring a unit holder's part home
/// can't keep them out for good.
contract UnservedUnitsFreezeTest is ClaimBase {
    function test_units_unserved_for_a_week_let_anyone_freeze_the_vault() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(150_000e6, 150_000e6, 1e18);

        // The venue stops answering: nothing can be valued or served.
        pinnedDriverActsAs(address(new FaultingDriver()));
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        Request memory r = vault.requestAt(id);
        vm.warp(r.earliest + 1 days + 1);
        vm.prank(alice);
        vault.takeProRata(id);
        (uint256 held,) = vault.unitsOf(alice);
        assertGt(held, 0, "alice holds no units");

        vm.expectRevert();
        vault.serveProRata(alice);

        vm.prank(bob);
        vm.expectRevert(IVault.NotYet.selector);
        vault.freezeUnits(alice);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.freezeUnits(bob);

        skip(7 days);
        vm.prank(bob);
        vault.freezeUnits(alice);
        assertTrue(vault.state() == VaultState.Frozen);
    }
}
