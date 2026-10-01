// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Fixture} from "./Fixture.sol";

/// A fallback whose fraction of the stations rounds to nothing leaves no
/// units behind, so it can never keep the station from deploying.
contract DustLeavesNoUnitsTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function test_a_one_unit_fallback_blocks_the_station_for_ever() public {
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        vm.prank(executor);
        station.pull(150_000e6);

        // Alice asks to redeem a single share unit, and nobody values the
        // vault for a day.
        vm.prank(alice);
        uint256 id = vault.requestRedeem(1, alice);
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRata(id);
        // A share unit's fraction of the station rounds to nothing, so it
        // leaves no units behind, and the station can deploy again.
        (uint256 total,) = vault.units();
        assertEq(total, 0, "a dust fraction left units that can never be cleared");
        value(vault);
        vm.prank(executor);
        station.pull(1);
    }
}
