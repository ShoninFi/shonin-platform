// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Prices} from "../src/Prices.sol";
import {Allowances, UnitPrices} from "../src/interfaces/IPrices.sol";
import {Fixture} from "./Fixture.sol";

/// How a fuel allowance becomes processor time on the bill: at the fuel a
/// core runs through in a second, fixed when the price list is deployed.
contract PricesFuelRateTest is Fixture {
    /// The asset has 6 decimals; prices are dollars scaled by 1e18.
    uint256 internal constant TO_ASSET = 1e12;

    /// A core's worth of fuel a second costs a core: $0.15 an hour, $3.60 a
    /// day, whatever else the allowances leave out.
    function test_a_core_is_billed_at_the_measured_fuel_rate() public view {
        assertEq(prices.fuelPerCoreSecond(), FUEL_PER_CORE_SECOND);
        Allowances memory a;
        a.fuelPerSec = FUEL_PER_CORE_SECOND;
        assertEq(prices.dailyCost(a), 3.6e6);
        assertEq(prices.costOf(a, 1000, 1000 + 1 hours), 0.15e6);
        a.fuelPerSec = FUEL_PER_CORE_SECOND / 4;
        assertEq(prices.dailyCost(a), 0.9e6);
    }

    /// A machine that runs through twice the fuel a second makes the same
    /// fuel half the processor time, so half the price.
    function test_a_faster_measured_rate_bills_the_same_fuel_less() public {
        Prices faster = new Prices(admin, PRICE_NOTICE, 2 * FUEL_PER_CORE_SECOND, publishedPrices());
        Allowances memory a;
        a.fuelPerSec = FUEL_PER_CORE_SECOND;
        assertEq(faster.dailyCost(a), prices.dailyCost(a) / 2);
    }

    /// A price list that couldn't say what a core is would divide by zero on
    /// every bill.
    function test_a_price_list_without_a_fuel_rate_is_refused() public {
        vm.expectRevert(Prices.NoFuelRate.selector);
        new Prices(admin, PRICE_NOTICE, 0, publishedPrices());
    }

    /// The fuel term alone, exactly: never a unit more than the fuel times
    /// the core price over the time, and less by under one unit.
    function testFuzz_fuel_is_never_billed_above_its_share_of_a_core(
        uint64 fuel,
        uint32 seconds_
    ) public view {
        fuel = uint64(bound(fuel, 0, 1000 * uint256(FUEL_PER_CORE_SECOND)));
        uint64 from = 1000;
        uint64 to = from + uint64(bound(seconds_, 0, 400 days));
        Allowances memory a;
        a.fuelPerSec = fuel;
        uint256 cost = prices.costOf(a, from, to);
        // Both sides scaled to whole units of the exact product.
        uint256 exact = uint256(fuel) * prices.current().vcpuHour * (to - from);
        uint256 per = uint256(FUEL_PER_CORE_SECOND) * 1 hours * TO_ASSET;
        assertLe(cost * per, exact, "never above the exact cost");
        assertGt((cost + 1) * per, exact, "within a unit below it");
    }

    /// Whatever the allowances and the interval, the cap is at most the sum
    /// of each allowance times its price, each rounded up: rounding only
    /// ever goes the tank's way.
    function testFuzz_the_cap_never_exceeds_allowances_times_prices(
        uint64 fuel,
        uint32 memoryMib,
        uint64 bytesPerDay,
        uint32 inferenceMsPerMin,
        uint64 gasPerDay,
        uint32 seconds_
    ) public view {
        Allowances memory a = Allowances({
            fuelPerSec: uint64(bound(fuel, 0, 1000 * uint256(FUEL_PER_CORE_SECOND))),
            memoryMib: uint32(bound(memoryMib, 0, 1 << 20)),
            requestsPerMin: 6000,
            bytesPerDay: uint64(bound(bytesPerDay, 0, 1e15)),
            streams: 64,
            inferenceMsPerMin: uint32(bound(inferenceMsPerMin, 0, 60_000)),
            gasPerDay: uint64(bound(gasPerDay, 0, 1e12))
        });
        uint256 s = bound(seconds_, 0, 400 days);
        UnitPrices memory p = prices.current();
        uint256 upper = _up(uint256(a.fuelPerSec) * p.vcpuHour * s, uint256(FUEL_PER_CORE_SECOND) * 1 hours)
            + _up(uint256(a.memoryMib) * p.gibHour * s, 1024 * 1 hours)
            + _up(uint256(a.bytesPerDay) * p.gbEgress * s, 1e9 * 1 days)
            + _up(uint256(a.inferenceMsPerMin) * p.inferenceSecond * s, 1000 * 60)
            + _up(uint256(a.gasPerDay) * TO_ASSET * s, 1 days);
        uint256 cost = prices.costOf(a, 1000, uint64(1000 + s));
        assertLe(cost * TO_ASSET, upper);
    }

    function _up(uint256 n, uint256 d) private pure returns (uint256) {
        return (n + d - 1) / d;
    }
}
