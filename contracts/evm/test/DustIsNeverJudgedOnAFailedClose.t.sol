// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Claim, ClaimState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A claim is judged dust only when its close went through and brought
/// nothing home, on what the stations hold at the mark or as the last priced
/// valuation counted them, whichever is more. A bid pushed outside the band
/// for one block, or an account under water at the bid, makes the close fail
/// and proves nothing: the claim waits with its shares intact.
contract DustIsNeverJudgedOnAFailedCloseTest is ClaimBase {
    /// Everything the station holds is at the venue: 190k of margin behind
    /// 30 BTC of perp at 60k (10x, inside the venue's 10% margin). Nothing is
    /// idle at the station, as a strategy that deploys its capital leaves it.
    function leveredPerp() internal {
        vm.startPrank(executor);
        station.pull(190_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 190_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 30e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        assertEq(usd.balanceOf(address(station)), 0, "the station keeps nothing idle");
        assertLt(venue.cashOf(address(station)), 0, "the perp is bought on margin");
    }

    /// The bid is pushed a sixth below the venue's own price for one block:
    /// far outside the 1% band, so a valuation now would be void and a
    /// close's orders would be refused. The account reads 190k at the mark
    /// and nothing at the bid.
    function test_a_bid_pushed_outside_the_band_for_one_block_leaves_the_claim_waiting() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        leveredPerp();
        uint256 id = claimFor(bob, bobShares);
        uint256 worthInBand = worth(bobShares);
        assertGt(worthInBand, 90_000e6, "bob's shares are worth about 100k inside the band");

        vm.prank(operator);
        venue.setPrices(PERP, PRICE, PRICE, PRICE * 5 / 6, PRICE);
        assertTrue(station.outsideBand(), "the market is outside the band");
        (uint256 atMark, uint256 atExit) = station.value();
        assertGt(atMark, 100_000e6, "worth something at the mark");
        assertEq(atExit, 0, "worth nothing at the pushed bid");
        int256 perpBefore = venue.positionOf(address(station), PERP).size;

        // Anyone settles the claim in that block.
        vm.prank(carol);
        vault.settleClaim(id);

        // The market is back the next block.
        setPrice(PERP, PRICE, 0);
        assertFalse(station.outsideBand());

        Claim memory c = vault.claimAt(id);
        assertEq(venue.positionOf(address(station), PERP).size, perpBefore, "something was sold");
        // Nobody is paid outside the band: the claim still waits with its
        // shares intact.
        assertTrue(c.state != ClaimState.Paid, "the claim was paid at a bid pushed outside the band");
        assertEq(c.shares, bobShares, "the claim's shares changed");
        worthInBand;
    }

    /// No manipulation at all: the market falls so the levered account is
    /// worth something at the mark and nothing at the bid, with the spread
    /// inside the band, and nothing idle at the station. The close is refused
    /// and the claim waits, as it does with idle dollars beside the account.
    function test_an_account_underwater_at_the_bid_still_holds_the_close() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        leveredPerp();
        uint256 id = claimFor(bob, bobShares);

        // BTC falls to 53,733: the account is worth about 2k at the mark; at
        // a bid 0.9% below it (inside the band) it is under water.
        setPrice(PERP, 53_733e6, 180);
        assertFalse(station.outsideBand(), "the market is inside the band");
        (uint256 atMark, uint256 atExit) = station.value();
        assertGt(atMark, 0, "worth something at the mark");
        assertEq(atExit, 0, "worth nothing at the bid");
        int256 perpBefore = venue.positionOf(address(station), PERP).size;

        vm.prank(carol);
        vault.settleClaim(id);

        Claim memory c = vault.claimAt(id);
        assertEq(venue.positionOf(address(station), PERP).size, perpBefore, "something was sold");
        assertTrue(c.state == ClaimState.Waiting, "the levered close was not held: the claim was settled");
        assertEq(c.shares, bobShares, "bob's pending shares were burned");
    }
}
