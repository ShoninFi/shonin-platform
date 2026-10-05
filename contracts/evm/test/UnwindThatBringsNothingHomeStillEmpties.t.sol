// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A stopped station's full unwind empties it even when it brings nothing
/// home. Here the station holds a levered perp account worth something at the
/// mark and nothing at the bid: the full unwind closes the position inside
/// the band, the proceeds only repay the account's debt, and `unwind(WAD)`
/// answers `(0, false)`. The station then reads as empty, and only then can
/// the vault close. So whoever keeps a halted vault moving must send that
/// unwind, not only one that would bring money home.
contract UnwindThatBringsNothingHomeStillEmptiesTest is ClaimBase {
    function setUp() public override {
        super.setUp();
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 50_000e6);
        // 120,000 at the venue, levered five times into a perp long.
        vm.startPrank(executor);
        station.pull(120_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 120_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 10e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        assertEq(venue.cashOf(address(station)), int256(120_000e6) - int256(600_000e6));

        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();

        // The market falls to where the account's margin is thinner than the
        // spread: $6,000 of equity at the mark, less than nothing at the bid,
        // with mark, bid and ask all inside the band around the venue's price.
        vm.prank(operator);
        venue.setPrices(PERP, 48_600e6, 48_200e6, 47_800e6, 48_600e6);
    }

    function atSlot() internal {
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
    }

    function test_an_unwind_that_brings_nothing_home_empties_the_station() public {
        (uint256 markBefore, uint256 exitBefore) = station.value();
        assertEq(markBefore, 6000e6, "worth something at the mark");
        assertEq(exitBefore, 0, "nothing at the bid");
        assertFalse(station.outsideBand(), "the market is inside the band");

        // Not blocked, and nothing home.
        (uint256 home, bool blocked) = station.unwind(1e18);
        assertEq(home, 0, "nothing came home");
        assertFalse(blocked, "the close was not blocked");

        // But the position is closed and the station is empty.
        assertEq(uint256(venue.positionOf(address(station), PERP).size), 0, "the position is closed");
        (uint256 markAfter,) = station.value();
        assertEq(markAfter, 0, "the station reads as empty");

        // And the vault now closes on the next slot's valuation.
        atSlot();
        vault.recordValuation();
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);
    }

    function test_without_that_unwind_a_valuation_never_finds_the_station_empty() public {
        atSlot();
        vault.recordValuation();
        vm.expectRevert();
        vault.close();
        assertFalse(vault.state() == VaultState.Closed);
    }
}
