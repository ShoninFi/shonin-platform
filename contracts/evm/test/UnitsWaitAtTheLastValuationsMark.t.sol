// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {IVault} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Units are judged dust at the mark, on the stations as they read now and
/// as the last priced valuation counted them, whichever is more. A levered
/// account the last valuation found worth something at the mark and nothing
/// at the bid, whose mark has since fallen to nothing, still counts for what
/// that valuation saw: a serve that brings nothing home leaves the units
/// waiting and keeps no sale, until a valuation prices the account at the
/// lower mark.
contract UnitsWaitAtTheLastValuationsMarkTest is ClaimBase {
    uint256 internal constant FALLEN = 54_208_754_208;
    uint256 internal constant EDGE_BID = 53_666_666_666;
    uint256 internal constant LOWER = 53_000e6;

    /// 30 BTC bought at 60k with 190k and 20 units of margin: at the edge
    /// bid the account is worth exactly nothing, at the fallen mark about
    /// 16,000 dollars.
    function leveredPerp() internal {
        vm.startPrank(executor);
        station.pull(190_000e6 + 20);
        station.approveKey("local", executor);
        station.deposit("local", 190_000e6 + 20);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 30e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        assertLt(venue.cashOf(address(station)), 0, "the perp is bought on margin");
    }

    function test_units_counted_at_the_last_valuations_mark_wait_while_the_market_now_reads_nothing()
        public
    {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        leveredPerp();
        value(vault);

        // Nothing prices for a day: bob takes the fallback and holds units
        // for his part of the station.
        setPrice(PERP, PRICE, 500);
        vm.prank(bob);
        uint256 id = vault.requestRedeem(bobShares, bob);
        vm.warp(vault.requestAt(id).earliest + 1 days);
        vm.prank(bob);
        vault.takeProRata(id);
        (uint256 units,) = vault.unitsOf(bob);
        assertGt(units, 0, "bob holds no units");

        // A valuation priced with the account worth something at the mark
        // and nothing at the bid.
        vm.prank(operator);
        venue.setPrices(PERP, FALLEN, FALLEN, EDGE_BID, FALLEN);
        assertFalse(station.outsideBand(), "the market is outside the band");
        (uint256 atMark, uint256 atExit) = station.value();
        assertGt(atMark, 0, "the account is worth nothing at the mark");
        assertEq(atExit, 0, "the account is worth something at the bid");
        value(vault);

        // The market falls further: the account is worth nothing at the
        // mark now too, and no valuation has priced it so.
        vm.prank(operator);
        venue.setPrices(PERP, LOWER, LOWER, LOWER, LOWER);
        (atMark,) = station.value();
        assertEq(atMark, 0, "the account is still worth something at the mark");
        int256 size = venue.positionOf(address(station), PERP).size;
        int256 cash = venue.cashOf(address(station));

        // The serve brings nothing home for units the last valuation counted
        // worth something: they wait, and nothing is sold for them.
        vm.expectRevert(IVault.NotYet.selector);
        vault.serveProRata(bob);
        (uint256 left,) = vault.unitsOf(bob);
        assertEq(left, units, "the units were cleared");
        assertEq(venue.positionOf(address(station), PERP).size, size, "a serve that paid nobody sold");
        assertEq(venue.cashOf(address(station)), cash, "a serve that paid nobody moved the venue's cash");

        // Once a valuation has counted the account at the lower mark, the
        // units are worth nothing and a serve clears them.
        value(vault);
        vault.serveProRata(bob);
        (left,) = vault.unitsOf(bob);
        assertEq(left, 0, "units worth nothing at every mark were not cleared");
    }
}
