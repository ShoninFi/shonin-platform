// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimBase} from "./ClaimBase.sol";

/// The pro-rata fallback charges the leaver's own part of the profit fee as
/// the last valuation's settlement left the pool, deposits included: after a
/// slot that took a large deposit, a fallback costs no more than a priced
/// exit at the same moment.
contract FallbackChargesTheFeeItsSharesOweTest is ClaimBase {
    function test_a_fallback_after_a_large_deposit_costs_no_more_than_a_priced_exit() public {
        depositAs(vault, alice, 100_000e6);
        // $90k at the venue in 1.5 BTC at $60k, then BTC +10%.
        deploySpot(90_000e6, 90_000e6, 1.5e18);
        setPrice(SPOT, PRICE * 11 / 10, 0);

        // Bob joins in the slot that prices the gain.
        requestDepositAs(vault, bob, 900_000e6);
        value(vault);

        uint256 aliceShares = vault.balanceOf(alice);
        vm.prank(alice);
        uint256 request = vault.requestRedeem(aliceShares, alice);

        // A day with nothing priced; land on a slot boundary so a valuation
        // could be recorded now.
        uint256 t = vault.requestAt(request).earliest + 24 hours + SLOT;
        vm.warp(t - (t % SLOT));

        // (a) What a priced exit pays her at this moment.
        uint256 snap = vm.snapshotState();
        uint256 before = usd.balanceOf(alice);
        vault.recordValuation();
        vault.settle(type(uint256).max);
        uint256 priced = usd.balanceOf(alice) - before;
        assertGt(priced, 100_000e6, "the priced exit paid");
        vm.revertToState(snap);

        // (b) What the fallback pays her: the cash share now, and her units
        // served at the same prices.
        before = usd.balanceOf(alice);
        vm.prank(alice);
        vault.takeProRata(request);
        vault.serveProRata(alice);
        vm.prank(alice);
        vault.collectUnits();
        uint256 fallen = usd.balanceOf(alice) - before;

        emit log_named_uint("priced exit", priced);
        emit log_named_uint("fallback   ", fallen);
        // No more than the fee her shares owe (a few dollars of rounding).
        assertGe(fallen + 5e6, priced, "the fallback charged more fee than her shares owed");
    }
}
