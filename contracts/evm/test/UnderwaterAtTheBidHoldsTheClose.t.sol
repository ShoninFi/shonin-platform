// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A levered account worth something at the venue's mark and nothing at its
/// exit side (underwater at the bid, inside the exit band) can't serve a
/// leaver: selling their fraction would bring them nothing and leave the
/// loss with everyone else. The claim's close is undone and the claim waits,
/// just as above water a close whose proceeds stay at the venue is undone.
contract UnderwaterAtTheBidHoldsTheCloseTest is ClaimBase {
    uint256 internal constant OFFSET = 1e12;

    /// Net worth per share at the venue's mark, after the profit fee owed:
    /// what the invariant handler's passive holder is measured by.
    function netPerShare() internal view returns (uint256) {
        (uint256 atMark,) = station.value();
        uint256 total = vault.cash() + atMark + 1;
        uint256 supply = vault.totalSupply() + OFFSET;
        uint256 basis = vault.mark() * supply / 1e30;
        uint256 fee = total > basis ? (total - basis) * 1500 / 10_000 : 0;
        return (total - fee) * 1e36 / supply;
    }

    function run(uint256 btc, bool underwater) internal {
        vm.prank(author);
        vault.setIdleFloor(0);
        skip(1 days);
        blessExecutorKey(executor, address(vault), bundle);

        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);

        // $20,000 at the venue, a perp of 3.3 BTC (about 10x) bought at
        // $60,000; everything else idle at the station, a dollar at home.
        vm.startPrank(executor);
        station.pull(vault.cash() - 1e6);
        station.approveKey("local", executor);
        station.deposit("local", 20_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 3.3e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();

        // BTC falls to $54,300, with a 1.8% spread around it (each side 0.9%
        // from the mark, inside the 1% band): the account is worth about
        // $1,190 at the mark and about -$420 at the bid.
        setPrice(PERP, btc, 180);
        assertFalse(station.outsideBand(), "the market is inside the band");
        (uint256 m, uint256 x) = venue.accountValue(address(station)) == 0
            ? (0, 0)
            : (venue.accountValue(address(station)), venue.exitValue(address(station)));
        assertGt(m, 0, "worth something at mark");
        if (underwater) assertEq(x, 0, "worth nothing (clamped from below zero) at the bid");
        else assertGt(x, 0, "worth something at the bid too");

        // Bob leaves with half his shares; the dollar at home can't pay it.
        uint256 id = claimFor(bob, bobShares / 2);
        uint256 stayers = netPerShare();
        int256 perpBefore = venue.positionOf(address(station), PERP).size;
        vault.settleClaim(id);
        // Underwater or not at the bid, nothing comes home for the leaver,
        // so the close is undone and the claim waits: nothing was sold, and
        // nobody's price moved.
        assertTrue(vault.claimAt(id).state == ClaimState.Waiting, "the levered close was not undone");
        assertEq(venue.positionOf(address(station), PERP).size, perpBefore, "something was sold");
        assertEq(netPerShare(), stayers, "the stayers' price moved");
    }

    /// Underwater at the bid: the close is refused before anything is sold.
    function test_a_claims_close_on_an_account_underwater_at_the_bid_waits_and_costs_nobody() public {
        run(54_300e6, true);
    }

    /// A little higher, still positive at the bid: the venue keeps the sale's
    /// proceeds as margin, so the close is undone too.
    function test_above_water_at_the_bid_the_levered_close_is_undone_too() public {
        run(54_600e6, false);
    }
}
