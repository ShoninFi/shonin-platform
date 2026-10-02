// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";

import {IStation as IStationErrors} from "../src/interfaces/IStation.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// The pro-rata fallback is a way out, never a way round the profit fee: a
/// claim that couldn't be served leaves with its share of the cash and units
/// for the rest, less the fee its shares owed at the last priced valuation,
/// paid as a slice of them to the author and the platform.
contract FallbackPaysTheProfitFeeTest is ClaimBase {
    /// 190k at the venue, two BTC long on the perp (120k notional against
    /// 190k): the account's cash is negative, so a partial close can never
    /// sweep its worth home.
    function leveredLong() internal {
        vm.startPrank(executor);
        station.pull(190_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 190_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 4e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        assertLt(venue.cashOf(address(station)), 0, "not levered");
    }

    function test_a_claim_on_a_levered_pool_above_its_mark_leaves_without_paying_the_profit_fee() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        leveredLong();
        // A fresh fee week, then BTC up 20%: the pool is 48k above its mark,
        // and a fee of 15% of that is owed.
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        skip(1 days);
        setPrice(SPOT, PRICE * 120 / 100, 0);
        setPrice(PERP, PRICE * 120 / 100, 0);
        value(vault);
        assertGt(latest().fee, 0, "no fee is owed");
        uint256 feeOwedByAlice = latest().feeExit * aliceShares / latest().supply;
        emit log_named_uint("profit fee alice's shares owe at this valuation", feeOwedByAlice);

        // Alice's withdrawal can't be paid from 10k of cash: a claim.
        uint256 id = claimFor(alice, aliceShares);

        // Nobody can serve it: the venue keeps the proceeds as margin.
        vm.prank(executor);
        vm.expectRevert(IStationErrors.ShortSweep.selector);
        station.unwind(0.5e18); // the fraction path, as a key would try it
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Waiting, "the claim was served");

        // A day later the fallback opens: her share of the cash, no fee,
        // and units for her share of the station.
        skip(1 days);
        uint256 authorBefore = usd.balanceOf(author) + vault.balanceOf(author);
        uint256 platformBefore = usd.balanceOf(platform) + vault.balanceOf(platform);
        uint256 bobBefore = perShare();
        vm.prank(alice);
        vault.takeProRataClaim(id);
        uint256 cashPart = usd.balanceOf(alice);

        // The strategy delevers (closes the perp), and anyone serves her
        // units: her fraction of the station at exit prices, no fee.
        blessExecutorKey(executor, address(vault), bundle);
        vm.startPrank(executor);
        station.approveKey("local", executor);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] = OrderRequest({
            market: PERP,
            isBuy: false,
            size: 4e18,
            limitPrice: PRICE * 120 / 100,
            reduceOnly: true
        });
        venue.place(address(station), orders);
        vm.stopPrank();
        vault.serveProRata(alice);
        vm.prank(alice);
        uint256 stationPart = vault.collectUnits();

        uint256 total = cashPart + stationPart;
        uint256 feeTaken = usd.balanceOf(author) + vault.balanceOf(author) - authorBefore
            + usd.balanceOf(platform) + vault.balanceOf(platform) - platformBefore;
        emit log_named_uint("alice got from the cash", cashPart);
        emit log_named_uint("alice got from the station", stationPart);
        emit log_named_uint("alice got in all", total);
        emit log_named_uint("fee paid to the author and platform", feeTaken);
        emit log_named_uint("bob's per-share worth before", bobBefore);
        emit log_named_uint("bob's per-share worth after", perShare());

        // The stayer is whole but for the day of holding fee the fallback
        // syncs: the leaver took exactly her fraction.
        assertGe(perShare() + bobBefore / 20_000, bobBefore, "bob paid for alice's exit");
        // But nothing of the fee her shares owed reached the fee recipients.
        assertGe(
            feeTaken,
            feeOwedByAlice / 2,
            "a leaver on a pool 24% above its mark left through the fallback and paid no profit fee"
        );
        bobShares;
    }
}
