// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Fixture} from "./Fixture.sol";

/// Serving one unit holder leaves the stations to everyone else in the same
/// proportions, so every remaining holder's fraction grows to match and the
/// next one served gets all of theirs.
contract UnitsRescaleAfterServeTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function test_the_second_unit_holder_served_gets_their_whole_fraction() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        // 150k at the station, 50k of cash.
        vm.prank(executor);
        station.pull(150_000e6);

        // Nothing is priced for a day; both leave through the fallback.
        vm.prank(alice);
        uint256 aliceRequest = vault.requestRedeem(aliceShares, alice);
        vm.prank(bob);
        uint256 bobRequest = vault.requestRedeem(bobShares, bob);
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRata(aliceRequest);
        vm.prank(bob);
        vault.takeProRata(bobRequest);

        // Each took their half of the cash and holds about half the station.
        (uint256 aliceUnits,) = vault.unitsOf(alice);
        (uint256 bobUnits,) = vault.unitsOf(bob);
        assertApproxEqRel(aliceUnits, 0.5e18, 0.01e18);
        assertApproxEqRel(bobUnits, 0.5e18, 0.01e18);
        assertApproxEqAbs(usd.balanceOf(alice), 25_000e6, 50e6);
        assertApproxEqAbs(usd.balanceOf(bob), 25_000e6, 50e6);

        // Serve alice: half of the 150k comes home for her.
        vault.serveProRata(alice);
        vm.prank(alice);
        uint256 toAlice = vault.collectUnits();
        assertApproxEqAbs(toAlice, 75_000e6, 100e6);

        // Serve bob: the other 75k is his. He is paid his "fraction" of the
        // 75k that is left, which is 37.5k.
        vault.serveProRata(bob);
        vm.prank(bob);
        uint256 toBob = vault.collectUnits();
        assertApproxEqAbs(toBob, 75_000e6, 100e6, "the second holder served was paid half of their money");
        assertLe(usd.balanceOf(address(station)), 100e6, "the station kept money nobody owns");
    }
}
