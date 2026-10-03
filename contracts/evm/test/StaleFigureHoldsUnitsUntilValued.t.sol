// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A liveness nit, recorded: units whose station has since been emptied by
/// a push are judged against the last priced valuation's figure, so they
/// can't be cleared until someone records a valuation; the units' freeze
/// clock keeps running meanwhile. Anyone can record one, so this is a
/// dependency on a valuation, not a freeze vector.
contract StaleFigureHoldsUnitsUntilValuedTest is ClaimBase {
    function test_units_of_an_emptied_station_wait_for_a_valuation_then_clear() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(150_000e6, 0, 0); // 150k idle at the station
        value(vault);
        vm.prank(alice);
        uint256 req = vault.requestRedeem(aliceShares, alice);
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRata(req);
        (uint256 held,) = vault.unitsOf(alice);
        assertGt(held, 0);
        value(vault); // the last priced valuation counts the station at 150k

        // The executor pushes everything home: alice's units get their part.
        vm.prank(executor);
        station.push(150_000e6);
        (, uint256 claimable) = vault.unitsOf(alice);
        assertApproxEqAbs(claimable, 150_000e6 * held / WAD, 2);
        (uint256 atMark,) = station.value();
        assertEq(atMark, 0, "the station is empty");

        // A serve now is refused: judged against the stale figure.
        vm.prank(bob);
        vm.expectRevert(IVault.NotYet.selector);
        vault.serveProRata(alice);

        // Without a valuation for a week, anyone could freeze the vault on
        // units that are worth nothing. A valuation (anyone's) clears them.
        skip(7 days);
        value(vault);
        vm.prank(bob);
        vault.serveProRata(alice);
        (uint256 after_,) = vault.unitsOf(alice);
        assertEq(after_, 0);
        assertTrue(vault.state() == VaultState.Open);
    }
}
