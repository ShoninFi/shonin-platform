// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Allowances, IPrices, PricePeriod, UnitPrices} from "./interfaces/IPrices.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// The published price list, and its history.
///
/// A vault caps what its tank can be charged at the author's allowances times
/// these prices, over the time that passed. Because the cap is worked out over
/// the history rather than at today's prices, a price change never reaches
/// back, and because a change waits out its notice, nobody's costs move
/// without warning.
contract Prices is IPrices {
    using Math for uint256;

    /// The asset has 6 decimals; prices are dollars scaled by 1e18.
    uint256 private constant TO_ASSET = 1e12;

    address public immutable admin;
    uint64 public immutable notice;

    PricePeriod[] private _history;

    error NotAdmin();
    error Backwards();

    constructor(address admin_, uint64 notice_, UnitPrices memory initial) {
        admin = admin_;
        notice = notice_;
        _history.push(PricePeriod({effectiveFrom: 0, prices: initial}));
        emit PricesScheduled(0, initial);
    }

    function schedule(UnitPrices calldata prices) external {
        if (msg.sender != admin) revert NotAdmin();
        uint64 effectiveFrom = uint64(block.timestamp) + notice;
        // With a fixed notice, every later schedule starts no earlier than an
        // earlier one, so the history stays in order.
        if (effectiveFrom < _history[_history.length - 1].effectiveFrom) revert Backwards();
        _history.push(PricePeriod({effectiveFrom: effectiveFrom, prices: prices}));
        emit PricesScheduled(effectiveFrom, prices);
    }

    function current() public view returns (UnitPrices memory) {
        for (uint256 i = _history.length; i > 0; i--) {
            if (_history[i - 1].effectiveFrom <= block.timestamp) return _history[i - 1].prices;
        }
        return _history[0].prices;
    }

    function history() external view returns (PricePeriod[] memory) {
        return _history;
    }

    function dailyCost(Allowances calldata allowances) external view returns (uint256) {
        return _cost(allowances, current(), 1 days) / TO_ASSET;
    }

    function costOf(Allowances calldata allowances, uint64 from, uint64 to) external view returns (uint256) {
        if (to <= from) return 0;
        uint256 total;
        uint256 n = _history.length;
        for (uint256 i = 0; i < n; i++) {
            uint64 start = _history[i].effectiveFrom;
            uint64 end = i + 1 < n ? _history[i + 1].effectiveFrom : type(uint64).max;
            // The overlap of [from, to) with this period.
            uint64 lo = from > start ? from : start;
            uint64 hi = to < end ? to : end;
            if (hi > lo) total += _cost(allowances, _history[i].prices, hi - lo);
        }
        return total / TO_ASSET;
    }

    /// What the allowances cost over `seconds` at `prices`, in dollars scaled
    /// by 1e18. Each term rounds down.
    function _cost(
        Allowances calldata a,
        UnitPrices memory p,
        uint256 seconds_
    ) private pure returns (uint256) {
        // A million units of fuel a second is one core, until the
        // fuel-to-processor mapping is measured.
        uint256 cores = (uint256(a.fuelPerSec) * p.vcpuHour).mulDiv(seconds_, 1e6 * 1 hours);
        uint256 memory_ = (uint256(a.memoryMib) * p.gibHour).mulDiv(seconds_, 1024 * 1 hours);
        uint256 egress = (uint256(a.bytesPerDay) * p.gbEgress).mulDiv(seconds_, 1e9 * 1 days);
        // Milliseconds of inference a minute, as seconds of inference a second.
        uint256 inference = (uint256(a.inferenceMsPerMin) * p.inferenceSecond).mulDiv(seconds_, 1000 * 60);
        uint256 gas = (uint256(a.gasPerDay) * TO_ASSET).mulDiv(seconds_, 1 days);
        return cores + memory_ + egress + inference + gas;
    }
}
