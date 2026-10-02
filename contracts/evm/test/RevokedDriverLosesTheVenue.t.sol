// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Fill, OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Fixture} from "./Fixture.sol";

/// Revoking a driver stops trading through it at once: the station prepares
/// nothing more through it, and anyone may withdraw the executor's key at the
/// venue straight away, blessed or not.
contract RevokedDriverLosesTheVenueTest is Fixture {
    function test_revoking_a_driver_stops_the_executor_trading_at_its_venue() public {
        (Vault vault,) = openVault();
        Station station = stationOf(vault);
        depositAs(vault, alice, 100_000e6);

        vm.startPrank(executor);
        station.pull(50_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 50_000e6);
        vm.stopPrank();
        assertTrue(venue.isAgent(address(station), executor));

        // The admin pulls the driver: urgent, immediate.
        vm.prank(admin);
        attestation.revokeDriver("local");
        assertEq(attestation.tradingDriver("local"), address(0));

        // The station refuses new work through it...
        vm.prank(executor);
        vm.expectRevert(Station.NotApproved.selector);
        station.deposit("local", 1);
        // ...and anyone may withdraw the venue key at once, blessed or not.
        vm.prank(alice);
        station.revokeKey("local", executor);

        // So the executor can no longer trade the account at the venue.
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 0.5e18, limitPrice: PRICE, reduceOnly: false});
        assertFalse(
            venue.isAgent(address(station), executor), "the venue agent survived the driver's revocation"
        );
        vm.prank(executor);
        vm.expectRevert();
        venue.place(address(station), orders);
    }
}
