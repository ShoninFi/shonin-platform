// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Claim, ClaimState, IVault} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A close that brings nothing home for a fraction worth something is never a
/// fill, however small the fraction's worth: it is undone, and a claim's
/// close that went through with nothing home reverts, so no sale at the bid
/// is kept while the claim waits.
contract NothingHomeIsNeverAFillTest is ClaimBase {
    uint256 internal constant OFFSET = 1e12;
    uint256 internal constant FALLEN = 54_208_754_208;
    uint256 internal constant EDGE_BID = 53_666_666_666;

    /// 30 BTC bought at 60k with 190k and `extra` units of margin: cash at
    /// the venue is -1,609,999,999,979 - ... + extra, so at the edge bid the
    /// account is worth `extra - 20` units.
    function leveredPerp(uint256 extra) internal {
        vm.startPrank(executor);
        station.pull(190_000e6 + extra);
        station.approveKey("local", executor);
        station.deposit("local", 190_000e6 + extra);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 30e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        assertEq(usd.balanceOf(address(station)), 0, "the station keeps nothing idle");
        assertLt(venue.cashOf(address(station)), 0, "the perp is bought on margin");
    }

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

    function arrange(uint256 extra) internal returns (uint256 id) {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        leveredPerp(extra);
        value(vault);
        id = claimFor(bob, bobShares);
        vm.prank(operator);
        venue.setPrices(PERP, FALLEN, FALLEN, EDGE_BID, FALLEN);
        assertFalse(station.outsideBand(), "the market is outside the band");
    }

    /// The guarded case, as a control: worth one unit at the bid, the
    /// leaver's half rounds to nothing and the close is refused outright.
    function test_worth_one_unit_at_the_bid_the_close_is_refused_and_sells_nothing() public {
        uint256 id = arrange(21);
        (, uint256 atExit) = station.value();
        assertEq(atExit, 1);
        int256 perpBefore = venue.positionOf(address(station), PERP).size;
        vm.prank(carol);
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Waiting);
        assertEq(venue.positionOf(address(station), PERP).size, perpBefore, "something was sold");
    }

    /// Two units more of venue cash: the account is worth 3 at the bid, the
    /// leaver's half is worth 1, and the same close goes through, sells half
    /// the pool's position at the bid, brings nothing home and is kept.
    function test_worth_three_units_at_the_bid_the_close_sells_nothing() public {
        uint256 id = arrange(23);
        (uint256 atMark, uint256 atExit) = station.value();
        assertEq(atExit, 3, "the account is not worth exactly three units at the bid");
        assertGt(atMark, 16_000e6, "the account is worth about 16k at the mark");
        int256 perpBefore = venue.positionOf(address(station), PERP).size;
        uint256 stayers = netPerShare();
        uint256 cashBefore = vault.cash();

        vm.prank(carol);
        vault.settleClaim(id);

        Claim memory c = vault.claimAt(id);
        (uint256 markAfter,) = station.value();
        emit log_named_int("perp size before", perpBefore);
        emit log_named_int("perp size after", venue.positionOf(address(station), PERP).size);
        emit log_named_uint("account at the mark before", atMark);
        emit log_named_uint("account at the mark after", markAfter);
        emit log_named_uint("stayers' net per share before (1e36)", stayers);
        emit log_named_uint("stayers' net per share after (1e36)", netPerShare());

        // By the vault's own account nothing was served: the claim waits, its
        // shares intact, and nothing came home.
        assertTrue(c.state == ClaimState.Waiting, "the claim was settled");
        assertEq(c.paid, 0, "the claim was paid");
        assertEq(vault.cash(), cashBefore, "something came home");
        // So it must have sold nothing (a failed close sells nothing;
        // no close that pays nobody). It sold half the position at the bid.
        assertEq(
            venue.positionOf(address(station), PERP).size,
            perpBefore,
            "a close that paid nobody sold the leaver's fraction of the pool's position at the bid"
        );
        assertEq(netPerShare(), stayers, "the stayers' worth at the mark moved");
    }

    /// Control: a serve of units on the same state is safe, because the
    /// vault reverts (`NotYet`) when nothing came home for units worth
    /// something, and the revert undoes the sale. Only the claim path keeps
    /// the sale, since "nothing came home" there is handled without a revert.
    function test_a_serve_on_the_same_state_reverts_and_the_sale_is_undone() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        leveredPerp(23);
        vm.prank(alice);
        uint256 request = vault.requestRedeem(aliceShares, alice);
        vm.prank(operator);
        venue.setPrices(PERP, PRICE, PRICE, PRICE * 5 / 6, PRICE);
        value(vault); // void
        skip(1 days + 2 * SLOT);
        vm.prank(alice);
        vault.takeProRata(request);
        vm.prank(operator);
        venue.setPrices(PERP, FALLEN, FALLEN, EDGE_BID, FALLEN);
        (, uint256 atExit) = station.value();
        assertEq(atExit, 3);
        int256 perpBefore = venue.positionOf(address(station), PERP).size;
        vm.prank(carol);
        vm.expectRevert(IVault.NotYet.selector);
        vault.serveProRata(alice);
        assertEq(venue.positionOf(address(station), PERP).size, perpBefore);
    }

    /// And it repeats whenever the account's bid-side worth lands back in
    /// the window (here one unit more of venue cash does it; on the stand-in
    /// the operator's bid can, for sizes with fine enough steps): each landing
    /// sells half of what is left at the bid, grinding the pool's equity at
    /// the mark into the spread while the claim never pays.
    function test_no_landing_in_the_window_sells_the_position() public {
        uint256 id = arrange(23);
        int256 perpBefore = venue.positionOf(address(station), PERP).size;
        (uint256 markBefore,) = station.value();
        for (uint256 i = 0; i < 3; i++) {
            vm.prank(carol);
            vault.settleClaim(id);
            assertTrue(vault.claimAt(id).state == ClaimState.Waiting, "the claim was settled");
            emit log_named_int("perp size after try", venue.positionOf(address(station), PERP).size);
            // The rounding of the sale left the account at 2 at the bid; a
            // unit more puts the leaver's half back at 1.
            give(address(station), 1);
            vm.prank(executor);
            station.deposit("local", 1);
            (, uint256 atExit) = station.value();
            assertGe(atExit, 3);
        }
        (uint256 markAfter,) = station.value();
        emit log_named_uint("account at the mark before", markBefore);
        emit log_named_uint("account at the mark after three tries", markAfter);
        assertEq(
            venue.positionOf(address(station), PERP).size,
            perpBefore,
            "three tries of a claim that paid nobody sold most of the pool's position"
        );
    }
}
