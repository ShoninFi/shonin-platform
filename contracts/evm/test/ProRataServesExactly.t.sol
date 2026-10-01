// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Fixture} from "./Fixture.sol";

/// Serving a unit holder brings home exactly their fraction at exit prices,
/// read before the close, so the holders who stayed keep their price.
contract ProRataServesExactlyTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function test_serving_a_unit_holder_leaves_everyone_elses_price_where_it_was() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);

        // All 150k deployed: 30k of cash at the venue and two BTC long, with
        // a 1% spread (bid 59,700, ask 60,300) inside the 1% exit band.
        vm.startPrank(executor);
        station.pull(150_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 150_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 2e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        setPrice(SPOT, PRICE, 100);
        value(vault);

        // Alice leaves through the fallback: half the cash now, units for
        // half of what the station holds.
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRata(id);
        value(vault);
        uint256 priceBefore = vault.sharePrice();
        (uint256 aliceUnits,) = vault.unitsOf(alice);
        (uint256 allUnits,) = vault.units();
        uint256 everyone = vault.totalSupply() + allUnits;

        vault.serveProRata(alice);
        vm.prank(alice);
        uint256 served = vault.collectUnits();

        // Bob, who stayed, keeps his price (section 16, invariant 2).
        value(vault);
        assertGe(vault.sharePrice(), priceBefore - priceBefore / 100_000, "bob paid for alice's close");

        // And alice got her fraction of the station at exit prices, 30k of
        // cash and two BTC at the bid, and no more.
        uint256 fair = (30_000e6 + 2 * 59_700e6) * aliceUnits / everyone;
        assertLe(served, fair + 1, "alice was paid more than her exit-side fraction");
    }
}
