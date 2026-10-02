// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {StationFactory} from "../src/StationFactory.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {FaultingDriver} from "./FaultingDriverEscape.t.sol";
import {FakeVault, StationBase} from "./Station.t.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A station keeps the drivers and bridges it was created with, and once
/// stopped it gets everything home that it can: each venue on its own, inside
/// a band that widens with time, and its idle dollars whatever a driver does.
contract PinnedDriversTest is StationBase {
    function test_a_bridge_approved_later_never_reaches_the_station() public {
        address pinned = station.pinnedBridge("local-bridge");
        assertEq(pinned, address(bridge));
        vm.prank(admin);
        attestation.proposeBridge("local-bridge", makeAddr("replacement bridge"));
        skip(TIMELOCK * 10);
        assertEq(station.pinnedBridge("local-bridge"), pinned, "the station's bridge changed");
    }

    function test_a_station_factory_refuses_a_zero_implementation() public {
        StationFactory fresh = new StationFactory(alice);
        vm.prank(alice);
        vm.expectRevert(StationFactory.NotInitialized.selector);
        fresh.initialize(address(attestation), address(usd), address(this), address(0));
    }

    function test_a_stopped_stations_band_holds_a_week_then_doubles_weekly() public {
        assertEq(station.stoppedBand(), 100, "a running station's band");
        stopped();
        assertEq(station.stoppedBand(), 100);
        skip(7 days - 1);
        assertEq(station.stoppedBand(), 100, "widened inside the first week");
        skip(1);
        assertEq(station.stoppedBand(), 200);
        skip(7 days);
        assertEq(station.stoppedBand(), 400);
        skip(7 days * 4);
        assertEq(station.stoppedBand(), 6400);
        skip(7 days);
        assertEq(station.stoppedBand(), 10_000, "never past any price");
        skip(365 days);
        assertEq(station.stoppedBand(), 10_000);
    }

    function test_a_market_outside_the_band_for_good_is_unwound_once_the_band_reaches_it() public {
        trading();
        // The spot market's bid sits 10% under its own price, for good.
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 90 / 100, PRICE);
        stopped();

        (, bool blocked) = station.unwind(1e18);
        assertTrue(blocked, "sold through a market outside the band");
        assertGt(venue.positionOf(address(station), SPOT).size, 0);

        // 2%, 4% and 8% still don't reach it; 16% does.
        skip(7 days * 3);
        (, blocked) = station.unwind(1e18);
        assertTrue(blocked, "sold before the band reached the market");
        skip(7 days);
        (, blocked) = station.unwind(1e18);
        assertFalse(blocked, "the market stayed out of reach");
        assertEq(venue.positionOf(address(station), SPOT).size, 0, "the position is still open");
        assertEq(venue.positionOf(address(station), PERP).size, 0, "the other market is still open");
        assertEq(usd.balanceOf(address(station)), 0, "money stayed at the station");
        assertGt(vault.cash(), 90_000e6, "the money did not come home");
    }

    function test_one_failing_driver_traps_nothing_else_at_a_stopped_station() public {
        FaultingDriver faulting = new FaultingDriver();
        vm.prank(admin);
        attestation.proposeDriver("broken", address(faulting));
        skip(TIMELOCK);
        bytes32[] memory names = new bytes32[](2);
        names[0] = "local";
        names[1] = "broken";
        FakeVault other = new FakeVault(IERC20(address(usd)));
        Station two = Station(ownStations.createHome(address(other), BUNDLE, names, bridges(), others(), 100));
        other.setStation(address(two));
        give(address(other), 100_000e6);
        other.fund(100_000e6);
        blessExecutorKey(executor, address(other), BUNDLE);

        vm.startPrank(executor);
        two.pull(100_000e6);
        two.approveKey("local", executor);
        two.deposit("local", 60_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 0.5e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(two), orders);
        vm.stopPrank();

        other.halt();
        skip(other.haltGrace());
        two.stop();
        (uint256 sentHome, bool blocked) = two.unwind(1e18);
        assertTrue(blocked, "a failed driver was reported as unwound");
        assertEq(venue.positionOf(address(two), SPOT).size, 0, "the working venue was not unwound");
        assertEq(usd.balanceOf(address(two)), 0, "idle dollars stayed at the station");
        assertGt(sentHome, 99_000e6, "the money did not come home");
    }

    function stopped() internal {
        vault.halt();
        skip(vault.haltGrace());
        station.stop();
    }
}
