// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, VaultState} from "../src/interfaces/IVault.sol";
import {TwoVenueVault} from "./TwoVenueVault.sol";

/// Dust at every venue adds up: the vault's dust threshold covers a remnant
/// at each of them at once, so units worth a few micro-dollars spread across
/// venues are served or cleared, and nobody can freeze the vault with them.
contract DustAtEveryVenueIsClearedTest is TwoVenueVault {
    address internal carol = makeAddr("carol");

    function arrange() internal returns (uint256 held) {
        uint256 aliceShares = depositAs(vault, alice, 150_000e6);
        depositAs(vault, bob, 50_000e6);

        // The station holds 7 units of the dollar: 3 at each venue as cash,
        // one idle. Everything else is cash at home.
        vm.startPrank(executor);
        station.pull(7);
        station.approveKey("local", executor);
        station.approveKey("local2", executor);
        station.deposit("local", 3);
        station.deposit("local2", 3);
        vm.stopPrank();
        (uint256 m1,) = driver.valueOf(address(station));
        (uint256 m2,) = driver2.valueOf(address(station));
        assertEq(m1, 3);
        assertEq(m2, 3);
        assertEq(usd.balanceOf(address(station)), 1);
        (uint256 atMark,) = station.value();
        assertEq(atMark, 7);
        assertFalse(station.outsideBand());

        // Nothing prices alice's request for a day; she takes the fallback:
        // three quarters of the cash now, and units for three quarters of
        // the station.
        vm.prank(alice);
        uint256 request = vault.requestRedeem(aliceShares, alice);
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRata(request);
        (held,) = vault.unitsOf(alice);
        assertApproxEqRel(held, 0.75e18, 0.02e18, "alice holds units for about three quarters");
        // Her part of the station at the mark: 5 units, above the 3 that
        // count as dust for a two-driver station.
        assertEq(atMark * held / WAD, 5);
        assertGt(vault.cash(), 49_000e6, "the vault keeps bob's cash at home");
    }

    function test_units_worth_five_micro_dollars_are_served_or_cleared() public {
        arrange();
        vm.prank(bob);
        (bool ok, bytes memory reason) = address(vault).call(abi.encodeCall(IVault.serveProRata, (alice)));
        if (!ok) emit log_named_bytes("serve refused with", reason);
        (uint256 units,) = vault.units();
        // A serve pays what it can and clears the rest as dust.
        assertTrue(ok, "a serve of dust units was refused");
        assertEq(units, 0, "dust units were left outstanding");
    }

    function test_nobody_can_freeze_the_vault_with_them() public {
        arrange();
        vm.prank(bob);
        vault.serveProRata(alice);
        skip(7 days);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.freezeUnits(alice);
        assertTrue(vault.state() == VaultState.Open, "the vault was frozen");
    }
}
