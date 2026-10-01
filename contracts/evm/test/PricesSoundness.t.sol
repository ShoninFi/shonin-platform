// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Allowances, PricePeriod, UnitPrices} from "../src/interfaces/IPrices.sol";
import {Fixture} from "./Fixture.sol";

/// Properties of `Prices` that were checked and found to hold, kept as
/// regression tests.
contract PricesSoundnessTest is Fixture {
    /// The Rust `Allowances::thinking()` shape, with no gas.
    function thinking() internal pure returns (Allowances memory a) {
        a.fuelPerSec = 4_000_000;
        a.memoryMib = 512;
        a.requestsPerMin = 120;
        a.bytesPerDay = 500_000_000;
        a.streams = 4;
        a.inferenceMsPerMin = 2000;
    }

    /// `shared::manifest::Prices::daily_cap` for the thinking shape:
    /// 4 × 0.15 × 24 + 0.5 × 0.01 × 24 + 0.5 × 0.15 + 2880 × 0.15/3600
    /// = 14.4 + 0.12 + 0.075 + 0.12 = 14.715. The chain holds the inference
    /// price truncated to 1e-18, so it lands one micro-dollar under.
    function test_a_day_at_the_thinking_shape_agrees_with_the_rust_formula() public view {
        uint256 cap = prices.dailyCost(thinking());
        assertApproxEqAbs(cap, 14_715_000, 1);
        assertLe(cap, 14_715_000, "rounds down, in the tank's favour");
    }

    /// Integration across three periods equals the sum of the pieces, and
    /// never exceeds the whole-interval figure at the dearest price.
    function test_cost_integrates_across_three_price_periods() public {
        Allowances memory a = thinking();
        uint64 t0 = uint64(block.timestamp);
        UnitPrices memory p2 = publishedPrices();
        p2.vcpuHour *= 3;
        UnitPrices memory p3 = publishedPrices();
        p3.gibHour = 0;
        p3.inferenceSecond *= 10;

        vm.prank(admin);
        prices.schedule(p2); // effective t0 + notice
        skip(5 days);
        vm.prank(admin);
        prices.schedule(p3); // effective t0 + 5 days + notice

        uint64 c1 = t0 + PRICE_NOTICE;
        uint64 c2 = t0 + 5 days + PRICE_NOTICE;
        uint64 from = c1 - 3 days;
        uint64 to = c2 + 2 days;

        uint256 whole = prices.costOf(a, from, to);
        uint256 pieces = prices.costOf(a, from, c1) + prices.costOf(a, c1, c2) + prices.costOf(a, c2, to);
        // Each period rounds down on its own inside `costOf` too, so the two
        // agree to within a unit per period.
        assertApproxEqAbs(whole, pieces, 3);

        // Each piece is a whole number of days at a known daily figure:
        // p2 is 4 × 0.45 × 24 + 0.12 + 0.075 + 0.12 = $43.515 a day;
        // p3 is 14.4 + 0 + 0.075 + 1.2 = $15.675 a day.
        vm.warp(c1); // p2 in force
        uint256 dayAtP2 = prices.dailyCost(a);
        assertApproxEqAbs(dayAtP2, 43_515_000, 1);
        assertApproxEqAbs(prices.costOf(a, c1, c2), 5 * dayAtP2, 5);
        assertApproxEqAbs(prices.costOf(a, from, c1), 3 * 14_715_000, 3);
        vm.warp(c2); // p3 in force
        assertApproxEqAbs(prices.dailyCost(a), 15_675_000, 1);
        assertApproxEqAbs(prices.costOf(a, c2, to), 2 * 15_675_000, 2);
        assertEq(prices.history().length, 3);
    }

    /// Two schedules in one block make a zero-length period; the later one
    /// wins everywhere, and the empty period contributes nothing.
    function test_two_schedules_in_one_block_leave_the_later_in_force() public {
        UnitPrices memory p2 = publishedPrices();
        p2.vcpuHour *= 2;
        UnitPrices memory p3 = publishedPrices();
        p3.vcpuHour *= 4;
        vm.startPrank(admin);
        prices.schedule(p2);
        prices.schedule(p3);
        vm.stopPrank();
        uint64 change = uint64(block.timestamp) + PRICE_NOTICE;

        skip(PRICE_NOTICE);
        assertEq(prices.current().vcpuHour, 4 * 0.15e18);
        Allowances memory a = allowancesCosting(0);
        a.fuelPerSec = 1_000_000; // one core: $3.60 a day, $14.40 at 4×
        assertEq(prices.costOf(a, change, change + 1 days), 14.4e6);
        assertEq(prices.costOf(a, change - 1 days, change), 3.6e6);
        PricePeriod[] memory h = prices.history();
        assertEq(h[1].effectiveFrom, h[2].effectiveFrom);
    }

    /// An empty or backwards interval costs nothing; a boundary is exact.
    function test_empty_and_boundary_intervals() public view {
        Allowances memory a = allowancesCosting(86_400); // one unit a second
        uint64 t = uint64(block.timestamp);
        assertEq(prices.costOf(a, t, t), 0);
        assertEq(prices.costOf(a, t + 1, t), 0);
        assertEq(prices.costOf(a, t, t + 1), 1);
        assertEq(prices.costOf(a, t, t + 1 days), 86_400);
    }

    /// A long history doesn't make a recent interval dear to price: only the
    /// periods it touches are read, and the answer is the same.
    function test_a_long_history_prices_a_recent_interval_the_same_and_cheaply() public {
        Allowances memory a = thinking();
        UnitPrices memory p = publishedPrices();
        for (uint256 i = 0; i < 200; i++) {
            p.vcpuHour += 1;
            vm.prank(admin);
            prices.schedule(p);
            skip(1 days);
        }
        skip(PRICE_NOTICE);
        uint64 from = uint64(block.timestamp);
        uint64 to = from + 1 days;
        uint256 before = gasleft();
        uint256 cost = prices.costOf(a, from, to);
        uint256 used = before - gasleft();
        assertLt(used, 60_000, "a recent day reads a handful of periods, not the whole history");
        assertApproxEqAbs(cost, prices.dailyCost(a), 1);
    }
}
