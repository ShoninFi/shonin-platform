// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Call} from "../../src/interfaces/IDriver.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// A close's measure is the account's worth now less the rest of its worth
/// then. Some spot holdings count for nothing (HYPE up to the reserve kept
/// for sends home, a holding worth less than the smallest order), and the
/// two sides treat them the same way: what was uncounted then stays
/// uncounted now, and nothing else. So neither a top-up of the reserve
/// bought during the stage, nor the rest of a holding the close sold part of
/// falling under the smallest order, lowers what the leaver is paid.
contract TheMeasureNeverChargesTheLeaverForWhatItDoesNotCountTest is HyperliquidVault {
    uint256 internal constant HALF = 0.5e18;

    function _band() internal view returns (uint16) {
        return station.spec().exitBandBps;
    }

    function test_a_top_up_of_the_reserve_bought_during_the_stage_costs_the_leaver_nothing() public {
        // $100 of spot dollars and 0.05 HYPE, under the reserve.
        core.creditSpot(address(station), 0, 100e8);
        core.creditSpot(address(station), HYPE_TOKEN, 0.05e8);
        int256[] memory before = hl.exposure(address(station));
        int256 dollars = hl.ledger(address(station));
        // The executor tops the reserve up with 0.6 HYPE at $20: $12 of the
        // dollars.
        core.buySpot(address(station), HYPE_PAIR, 60, 20_000_000);
        int256 measured = hl.measure(address(station), dollars, before, HALF, 1e18, _band());
        assertEq(measured, 50e6, "half of the $100 the account was worth, the top-up charged to nobody");
    }

    function test_the_rest_of_a_holding_sold_in_part_counts_however_small() public {
        // Two HYPE: one above the reserve, $24.
        core.creditSpot(address(station), HYPE_TOKEN, 2e8);
        core.setSpot(HYPE_PAIR, 24_000_000, 24_000_000, 24_000_000);
        int256[] memory before = hl.exposure(address(station));
        int256 dollars = hl.ledger(address(station));
        // Half of it sold, as the close orders it.
        (Call[] memory calls, bool blocked) = hl.close(address(station), HALF, _band());
        assertFalse(blocked, "the close is ordered");
        assertEq(calls.length, 1, "one sale");
        vm.prank(address(station));
        (bool ok,) = calls[0].target.call(calls[0].data);
        assertTrue(ok, "queued");
        _nextBlock();
        int256 proceeds = hl.ledger(address(station)) - dollars;
        assertEq(proceeds, 12e6, "half a HYPE sold at $24");
        // The price falls: the half left is worth $9.50, under the smallest
        // order.
        core.setSpot(HYPE_PAIR, 19_000_000, 19_000_000, 19_000_000);
        int256 measured = hl.measure(address(station), dollars, before, HALF, 1e18, _band());
        assertEq(measured, proceeds, "the leaver is paid what the sale fetched, nothing taken for the rest");
    }
}
