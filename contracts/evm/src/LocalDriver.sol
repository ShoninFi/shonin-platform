// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Call, Custody, ExitPath, IDriver, ValuationKind} from "./interfaces/IDriver.sol";
import {ILocalVenue, Market, OrderRequest, Position} from "./interfaces/ILocalVenue.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// The stand-in venue's driver, for local chains and testnets.
///
/// It is built on the same interface a real venue's driver is, so every path a
/// real one takes -- approving a key, moving dollars in and out, closing a
/// leaver's fraction inside the band, valuing the account -- runs here before
/// a real venue exists. The station is the venue account's master.
contract LocalDriver is IDriver {
    using Math for uint256;

    uint256 private constant WAD = 1e18;
    uint256 private constant BPS = 10_000;

    ILocalVenue public immutable venue;
    IERC20 public immutable asset;

    constructor(ILocalVenue venue_, IERC20 asset_) {
        venue = venue_;
        asset = asset_;
    }

    function name() external pure returns (bytes32) {
        return "local";
    }

    function targets() external view returns (address[] memory list) {
        list = new address[](1);
        list[0] = address(venue);
    }

    /// The stand-in venue's terms exclude nobody; the vault's baseline still
    /// applies.
    function restricted() external pure returns (bytes32[] memory) {
        return new bytes32[](0);
    }

    function custody() external pure returns (Custody) {
        return Custody.Venue;
    }

    function exitPath() external pure returns (ExitPath) {
        return ExitPath.Contract;
    }

    function valuation() external pure returns (ValuationKind) {
        return ValuationKind.OnChain;
    }

    function approveKey(address, address key, uint64 validUntil) external view returns (Call[] memory) {
        return _one(abi.encodeCall(ILocalVenue.approveAgent, (key, validUntil)));
    }

    function revokeKey(address, address key) external view returns (Call[] memory) {
        return _one(abi.encodeCall(ILocalVenue.revokeAgent, (key)));
    }

    function deposit(address, uint256 amount) external view returns (Call[] memory calls) {
        calls = new Call[](2);
        calls[0] = Call(address(asset), 0, abi.encodeCall(IERC20.approve, (address(venue), amount)));
        calls[1] = Call(address(venue), 0, abi.encodeCall(ILocalVenue.deposit, (amount)));
    }

    function withdraw(address, uint256 amount) external view returns (Call[] memory) {
        return _one(abi.encodeCall(ILocalVenue.withdraw, (amount)));
    }

    function close(
        address station,
        uint256 fraction,
        uint16 bandBps
    ) external view returns (Call[] memory calls, bool blocked) {
        bytes32[] memory held = venue.marketsHeldBy(station);
        OrderRequest[] memory orders = new OrderRequest[](held.length);
        uint256 count;
        for (uint256 i = 0; i < held.length; i++) {
            Position memory position = venue.positionOf(station, held[i]);
            if (position.size == 0) continue;
            bool isLong = position.size > 0;
            uint256 open = uint256(isLong ? position.size : -position.size);
            uint256 size = open.mulDiv(fraction, WAD, Math.Rounding.Ceil);
            if (size > open) size = open;
            if (size == 0) continue;

            Market memory market = venue.marketOf(held[i]);
            // The full band accepts any price: a stopped station that has
            // waited long enough sells at whatever the market offers.
            if (bandBps >= BPS) {
                orders[count++] = OrderRequest({
                    market: held[i],
                    isBuy: !isLong,
                    size: size,
                    limitPrice: isLong ? 0 : type(uint256).max,
                    reduceOnly: true
                });
                continue;
            }
            // A long is sold at the bid, a short bought back at the ask, and
            // neither may be worse than the band around the venue's own price.
            uint256 limit = isLong
                ? market.oracle.mulDiv(BPS - bandBps, BPS, Math.Rounding.Ceil)
                : market.oracle.mulDiv(BPS + bandBps, BPS, Math.Rounding.Floor);
            if (isLong ? market.bid < limit : market.ask > limit) return (new Call[](0), true);
            // Nor while the venue's mark, which values the position, sits
            // further from its own price than the band on either side: a
            // valuation there would price everyone at a mark nobody trusts.
            uint256 low = market.oracle.mulDiv(BPS - bandBps, BPS, Math.Rounding.Ceil);
            uint256 high = market.oracle.mulDiv(BPS + bandBps, BPS, Math.Rounding.Floor);
            if (market.mark < low || market.mark > high) return (new Call[](0), true);

            orders[count++] = OrderRequest({
                market: held[i],
                isBuy: !isLong,
                size: size,
                limitPrice: limit,
                reduceOnly: true
            });
        }
        if (count == 0) return (new Call[](0), false);
        assembly {
            mstore(orders, count)
        }
        calls = _one(abi.encodeCall(ILocalVenue.place, (station, orders)));
    }

    function sweep(address station, uint256 amount) external view returns (Call[] memory) {
        int256 cash = venue.cashOf(station);
        if (cash <= 0) return new Call[](0);
        if (amount > uint256(cash)) amount = uint256(cash);
        if (amount == 0) return new Call[](0);
        return _one(abi.encodeCall(ILocalVenue.withdraw, (amount)));
    }

    function valueOf(address station) external view returns (uint256 mark, uint256 exit) {
        mark = venue.accountValue(station);
        exit = venue.exitValue(station);
    }

    function _one(bytes memory data) private view returns (Call[] memory calls) {
        calls = new Call[](1);
        calls[0] = Call(address(venue), 0, data);
    }
}
