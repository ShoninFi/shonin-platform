// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Valuation} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A venue's bid above its mark is never paid out and never charged a fee:
/// withdrawals and fees use the mark, and only a deposit pays the bid's
/// premium on top, so a one-block bid takes nothing from those who stay.
contract BidAboveTheMarkTest is ClaimBase {
    uint256 internal aliceShares;
    uint256 internal bobShares;
    uint256 internal carolShares;

    /// Three holders of a third each. 100k of the 300k sits at the venue in
    /// spot BTC (1.5 BTC at 60k, plus 10k of venue cash); 200k is cash.
    function thirds() internal {
        aliceShares = depositAs(vault, alice, 100_000e6);
        bobShares = depositAs(vault, bob, 100_000e6);
        carolShares = depositCarol(100_000e6);
        deploySpot(100_000e6, 100_000e6, 1.5e18);
        value(vault);
        assertApproxEqAbs(vault.totalValue(), 300_000e6, 1e6);
    }

    /// What `shares` own at the venue's mark, which is where the pool still
    /// holds its position once the bid goes back.
    function atMark(uint256 shares) internal view returns (uint256) {
        (uint256 mark,) = station.value();
        return (vault.cash() + mark) * shares / vault.totalSupply();
    }

    function test_a_one_block_bid_above_the_mark_pays_a_leaver_no_more_than_the_mark() public {
        thirds();
        uint256 bobBefore = atMark(bobShares);
        uint256 aliceMarkWorth = atMark(aliceShares);

        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);

        // In the block the valuation is recorded, the venue's bid sits 50%
        // above its own mark and oracle. Nothing is sold at it.
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 150 / 100, PRICE * 151 / 100);
        value(vault);
        Valuation memory v = latest();
        // Valued at the mark for withdrawals and fees: the bid above it is
        // only what a deposit would pay on top.
        assertEq(v.gross, v.cash + 100_000e6 + 1, "the bid was counted as value");
        assertLe(v.grossExit, v.gross);

        // The bid is gone the next block.
        setPrice(SPOT, PRICE, 0);

        uint256 paid = usd.balanceOf(alice);
        emit log_named_uint("alice's worth at mark", aliceMarkWorth);
        emit log_named_uint("alice was paid from cash", paid);
        emit log_named_uint("bob's worth at mark before", bobBefore);
        emit log_named_uint("bob's worth at mark after", atMark(bobShares));

        // She was paid no more than the mark value of her shares.
        assertLe(paid, aliceMarkWorth, "a leaver was paid from cash above the mark value of her shares");
        // And the holders who stay bear it.
        assertGe(atMark(bobShares) + 1e6, bobBefore, "the stayers paid for a bid nobody sold at");
    }

    function test_a_one_block_bid_above_the_mark_at_payday_mints_no_fee_on_it() public {
        thirds();
        uint256 bobBefore = atMark(bobShares);
        assertEq(vault.balanceOf(author), 0);
        assertEq(vault.balanceOf(platform), 0);

        // The first valuation on or after Monday is payday. The venue's bid
        // sits 50% above its mark in that one block.
        vm.warp(nextMondayAfter(vm.getBlockTimestamp()));
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 150 / 100, PRICE * 151 / 100);
        value(vault);
        setPrice(SPOT, PRICE, 0);
        value(vault);

        emit log_named_uint("fee shares minted to the author", vault.balanceOf(author));
        emit log_named_uint("fee shares minted to the platform", vault.balanceOf(platform));
        emit log_named_uint("bob's worth at mark before payday", bobBefore);
        emit log_named_uint("bob's worth at mark after payday", atMark(bobShares));
        emit log_named_uint("the mark, raised to the bid", vault.mark());

        // Nothing was gained, so nothing is owed: a profit fee is paid on
        // what the vault is worth above its mark, and at mark it is worth
        // what it was.
        // The only profit is the transaction fees the three deposits left in
        // the pool (0.1% of 300k), on which 10% goes to the author.
        assertLe(vault.balanceOf(author) * atMark(1e18) / 1e18, 31e6, "the author was paid a fee on a bid");
        // Bob bears his third of that fee on the kept transaction fees (about
        // $15) and nothing on the bid.
        assertGe(atMark(bobShares) + 20e6, bobBefore, "the stayers paid a fee on a gain never realised");
    }
}
