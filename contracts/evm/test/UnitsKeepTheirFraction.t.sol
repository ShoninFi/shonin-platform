// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Fixture} from "./Fixture.sol";

/// A fallback leaver's units are their fraction of the stations, fixed when
/// they left, so fee shares minted later for the shares' gains don't dilute
/// them.
contract UnitsKeepTheirFractionTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    /// Move to the first Monday after the last payday, never backwards: if
    /// that Monday has passed, the next valuation is payday anyway.
    function toPayday() internal {
        uint256 monday = nextMondayAfter(vault.lastPayment()) + 1;
        if (monday > vm.getBlockTimestamp()) vm.warp(monday);
    }

    function nextMondayAfter(uint256 t) internal pure returns (uint256) {
        uint256 day = t / 1 days;
        return (day - ((day + 3) % 7)) * 1 days + 7 days;
    }

    function test_a_payday_takes_from_the_unit_holders() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        vm.prank(executor);
        station.pull(150_000e6);

        // Alice leaves through the fallback: her share of the 50k of cash
        // now, and units for her half of what the station holds.
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRata(id);
        (uint256 units,) = vault.units();
        assertGt(units, 0.49e18);
        assertLt(units, 0.51e18);

        // The station gains 30k, and payday comes.
        toPayday();
        value(vault);
        give(address(station), 30_000e6);
        toPayday();
        value(vault);
        assertGt(vault.balanceOf(author), 0, "the profit fee was paid in shares");

        // Everything comes home. Alice's units are owed their fraction of
        // the station as it stood when she left: half of 180k, give or
        // take the holding fee for the week.
        blessExecutorKey(executor, address(vault), bundle);
        vm.prank(executor);
        station.push(180_000e6);
        (, uint256 claimable) = vault.unitsOf(alice);
        uint256 fair = 180_000e6 * units / 1e18;
        assertApproxEqAbs(claimable, fair, 100e6, "the unit holders paid for the shares' fee");
    }
}
