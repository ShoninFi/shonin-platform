// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Claim, ClaimState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A fractional close is refused when the leaver's fraction of an account is
/// worth something at the mark and nothing at exit prices, however close to
/// zero the account is: selling it would bring the leaver nothing and leave
/// the loss with everyone else.
contract NoCloseThatPaysNobodyTest is ClaimBase {
    /// The venue's own price at which 30 BTC bought at 60k on 10% margin is
    /// worth about 16k at the mark, and the bid at the band's edge below it,
    /// chosen so the account is worth exactly one unit at the bid.
    uint256 internal constant FALLEN = 54_208_754_208;
    uint256 internal constant EDGE_BID = 53_666_666_666;

    function leveredPerp() internal {
        vm.startPrank(executor);
        station.pull(190_000e6 + 21);
        station.approveKey("local", executor);
        station.deposit("local", 190_000e6 + 21);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 30e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        assertEq(usd.balanceOf(address(station)), 0, "the station keeps nothing idle");
        assertEq(venue.cashOf(address(station)), -1_609_999_999_979, "the perp is bought on margin");
    }

    function test_a_close_that_pays_nobody_never_sells_the_pools_position() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        leveredPerp();
        value(vault);
        uint256 id = claimFor(bob, bobShares);

        // The market falls and the bid sits at the band's edge: inside the
        // band, so a valuation now would price and a close's orders fill.
        vm.prank(operator);
        venue.setPrices(PERP, FALLEN, FALLEN, EDGE_BID, FALLEN);
        assertFalse(station.outsideBand(), "the market is outside the band");
        (uint256 atMark, uint256 atExit) = station.value();
        assertEq(atExit, 1, "the account is not worth exactly one unit at the bid");
        assertGt(atMark, 16_000e6, "the account is worth about 16k at the mark");
        int256 perpBefore = venue.positionOf(address(station), PERP).size;

        // Anyone tries the claim.
        vm.prank(carol);
        vault.settleClaim(id);

        Claim memory c = vault.claimAt(id);
        (uint256 markAfter,) = station.value();
        emit log_named_int("perp size before", perpBefore);
        emit log_named_int("perp size after", venue.positionOf(address(station), PERP).size);
        emit log_named_uint("account at the mark before", atMark);
        emit log_named_uint("account at the mark after", markAfter);
        emit log_named_uint("claim paid", c.paid);

        // The claim was paid nothing and still waits with its shares intact:
        // by the vault's own rule that close was "not a try" ...
        assertTrue(c.state == ClaimState.Waiting, "the claim was settled");
        assertEq(c.paid, 0, "the claim was paid");
        assertEq(c.shares, bobShares, "the claim's shares changed");
        // ... so it must have sold nothing. It sold half the position at the
        // bid, and the pool lost half its equity at the mark to the spread.
        assertEq(
            venue.positionOf(address(station), PERP).size,
            perpBefore,
            "a close that paid nobody sold the leaver's fraction of the pool's position at the bid"
        );
    }
}
