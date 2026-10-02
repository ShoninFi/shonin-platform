// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {IVault, Request} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A unit holder's serve waits, as a claim does, while a priced valuation
/// waits to settle.
contract ServeWaitsForSettlementTest is ClaimBase {
    function test_a_serve_on_a_stopped_station_waits_for_settlement() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 3e18);

        // Alice leaves through the fallback after a day with nothing priced
        // (the venue is held outside the band for that day), so she holds
        // units for her part of the station.
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        setPrice(SPOT, PRICE, 300);
        Request memory r = vault.requestAt(id);
        vm.warp(r.earliest + 1 days + 1);
        vm.prank(alice);
        vault.takeProRata(id);
        (uint256 held,) = vault.unitsOf(alice);
        assertGt(held, 0, "alice holds no units");
        setPrice(SPOT, PRICE, 0);

        // The vault is killed and the station stopped.
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        assertTrue(station.stopped());

        // Bob's withdrawal is priced and left unsettled, as anyone may do.
        uint256 bobShares = vault.balanceOf(bob);
        vm.prank(bob);
        vault.requestRedeem(bobShares / 2, bob);
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        assertTrue(vault.unsettled(), "nothing waits to settle");

        // A stranger's full unwind waits for settlement ...
        vm.prank(carol);
        vm.expectRevert(Station.NotAllowed.selector);
        station.unwind(1e18);

        // ... and so, the handbook says, does a serve of alice's units.
        vm.prank(carol);
        vm.expectRevert(IVault.NotYet.selector);
        vault.serveProRata(alice);
    }
}
