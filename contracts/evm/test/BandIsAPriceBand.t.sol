// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState, Valuation} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// The band rule is a price band, as a close's is: a tight spread on the
/// venue's price doesn't stop a withdrawal the cash covers, however levered
/// the book, where comparing the station's equity would multiply it.
contract BandIsAPriceBandTest is ClaimBase {
    uint256 internal constant BPS = 10_000;

    /// Alice and bob hold half each. Half the vault is cash; the other half
    /// backs a 10x perp long (10 BTC at 60k on 100k of margin), which is the
    /// leverage the stand-in venue allows.
    function halfInALeveredPerp() internal returns (uint256 aliceShares, uint256 bobShares) {
        aliceShares = depositAs(vault, alice, 100_000e6);
        bobShares = depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(100_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 100_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 10e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        value(vault);
        assertApproxEqAbs(vault.totalValue(), 200_000e6, 1e6);
    }

    function test_a_tight_spread_on_a_levered_book_stops_no_withdrawal_the_cash_covers() public {
        (uint256 aliceShares,) = halfInALeveredPerp();

        // A 0.4% spread: the bid is 0.2% below the venue's own price, well
        // inside the 1% band a close is allowed to fill in.
        setPrice(PERP, PRICE, 40);
        uint256 bid = venue.marketOf(PERP).bid;
        uint256 limit = venue.marketOf(PERP).oracle * (BPS - vault.exitBandBps()) / BPS;
        assertGe(bid, limit, "the bid is outside the band a close may fill in");
        (uint256 mark, uint256 exit) = station.value();
        emit log_named_uint("bid", bid);
        emit log_named_uint("lowest bid a close may fill at", limit);
        emit log_named_uint("station equity at mark", mark);
        emit log_named_uint("station equity at exit", exit);

        // Alice asks for her half back; 100k of cash sits idle for it.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        Valuation memory v = latest();
        emit log_named_uint("valuation gross", v.gross);
        emit log_named_uint("valuation grossExit", v.grossExit);

        // Whatever the valuation did, trying the claim once and valuing
        // again, at the same tight spread, must pay her.
        if (vault.claimCount() != 0) {
            vault.settleClaim(1);
            value(vault);
        }
        assertGt(
            usd.balanceOf(alice), 0, "a withdrawal the cash covers was not paid at a spread inside the band"
        );
    }
}
