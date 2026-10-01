// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {IStation} from "../src/interfaces/IStation.sol";
import {Fixture} from "./Fixture.sol";

/// A unit holder's units are only burned for a close that brings their
/// worth home. While a venue keeps the proceeds as margin, serving them is
/// refused and they keep their units.
contract ServeNeedsItsWorthHomeTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    /// 150k at the venue and five BTC long on the perp: 300k of notional
    /// against 150k, inside the venue's 10% margin, with the account's cash
    /// at -150k.
    function leveredLong() internal {
        vm.startPrank(executor);
        station.pull(150_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 150_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 5e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        assertLt(venue.cashOf(address(station)), 0);
    }

    function test_a_unit_holder_is_not_served_while_their_worth_stays_at_the_venue() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        leveredLong();

        // Nothing is priced for a day: alice takes half the cash and units
        // for half of what the station holds.
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRata(id);
        (uint256 held,) = vault.unitsOf(alice);
        assertApproxEqRel(held, 0.5e18, 0.01e18);
        (, uint256 stationExit) = station.value();
        uint256 fair = stationExit * held / 1e18;
        assertApproxEqAbs(fair, 75_000e6, 500e6);

        // Bob tries to serve her. The close would fill inside the band, but
        // the account's cash would still be negative, so nothing could come
        // home: the serve is refused.
        vm.prank(bob);
        vm.expectRevert(IStation.ShortSweep.selector);
        vault.serveProRata(alice);

        (uint256 heldAfter, uint256 claimable) = vault.unitsOf(alice);
        assertTrue(
            heldAfter != 0 || claimable + 100e6 >= fair,
            "her units were burned for a close that brought her nothing"
        );
    }
}
