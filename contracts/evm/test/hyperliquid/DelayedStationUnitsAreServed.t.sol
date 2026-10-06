// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Stage} from "../../src/interfaces/IStation.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// A holder who took the pro-rata fallback holds units for their fraction of
/// what the station holds, and `serveProRata` closes that fraction and gives
/// them what came home. On a station whose closes settle after the block the
/// serve stages a close for that holder, and its delivery is credited to them
/// alone, so their units are served rather than left to freeze the vault.
contract DelayedStationUnitsAreServedTest is HyperliquidVault {
    function test_units_left_by_the_fallback_are_served_on_a_delayed_station() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        vm.prank(alice);
        uint256 request = vault.requestRedeem(aliceShares, alice);

        // Nothing prices the withdrawal for a day: Alice takes the fallback,
        // her share of the cash now and units for the rest.
        skip(1 days + 1 minutes);
        vm.prank(alice);
        vault.takeProRata(request);
        (uint256 units,) = vault.unitsOf(alice);
        assertGt(units, 0, "she holds units for her part of the station");

        vault.serveProRata(alice);
        Stage memory staged = station.stage();
        assertEq(staged.fraction, units, "a close of exactly her fraction is staged for her");
        assertTrue(staged.forVault);

        _settleStaged();
        assertEq(station.stage().stagedAt, 0, "delivered");
        (uint256 left, uint256 claimable) = vault.unitsOf(alice);
        assertEq(left, 0, "her units are served");
        assertApproxEqRel(claimable, 95_000e6, 0.01e18, "credited what her close fetched");
        (uint256 total, uint256 reserve) = vault.units();
        assertEq(total, 0);
        assertEq(reserve, claimable, "nothing else took any of it");
        vm.prank(alice);
        vault.collectUnits();
    }
}
