// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {
    Call,
    Custody,
    ExitPath,
    IDelayedDriver,
    IDriver,
    ValuationKind
} from "../../../../../contracts/evm/src/interfaces/IDriver.sol";
import {LighterReport, LighterShares} from "../../../../../contracts/evm/src/drivers/lighter/LighterBoard.sol";
import {Scripted} from "./Scripted.sol";

/// A driver as the keeper reads it, for the keeper's differential tests: its
/// name and exit path, and, for one whose venue acts after the transaction,
/// the reads the keeper makes of a Lighter driver (its exchange, its dollar
/// asset, its board, what a venue vault holds). Every call list is empty.
///
/// Script keys: `name`, `exitPath`, `exchange`, `board`, `dollarAsset`,
/// `equity`/held (what the station holds in a venue vault).
contract StandInDriver is Scripted, IDriver, IDelayedDriver {
    function name() external view returns (bytes32) {
        return bytes32(_get("name"));
    }

    function exitPath() external view returns (ExitPath) {
        return ExitPath(_get("exitPath"));
    }

    function exchange() external view returns (address) {
        return _addr(_get("exchange"));
    }

    function board() external view returns (address) {
        return _addr(_get("board"));
    }

    function DOLLAR_ASSET() external view returns (uint16) {
        return uint16(_get("dollarAsset"));
    }

    function reclaim(address, address held) external view returns (Call[] memory calls, uint256 equity) {
        return (calls, _get("equity", _word(held)));
    }

    function custody() external pure returns (Custody) {
        return Custody.Venue;
    }

    function valuation() external pure returns (ValuationKind) {
        return ValuationKind.OnChain;
    }

    function targets() external pure returns (address[] memory) {}

    function restricted() external pure returns (bytes32[] memory) {}

    function approveKey(address, address, uint64) external pure returns (Call[] memory) {}

    function revokeKey(address, address) external pure returns (Call[] memory) {}

    function deposit(address, uint256) external pure returns (Call[] memory) {}

    function withdraw(address, uint256) external pure returns (Call[] memory) {}

    function close(address, uint256, uint16) external pure returns (Call[] memory calls, bool blocked) {}

    function sweep(address, uint256) external pure returns (Call[] memory) {}

    function valueOf(address) external pure returns (uint256, uint256) {}

    function exposure(address) external pure returns (int256[] memory) {}

    function ledger(address) external pure returns (int256) {}

    function closedPart(address, int256[] calldata, uint256, uint64) external pure returns (uint256, bool) {}

    function measure(address, int256, int256[] calldata, uint256, uint256, uint16) external pure returns (int256) {}

    function deliveryWindow() external pure returns (uint64) {
        return 600;
    }

    function settleDelay() external pure returns (uint64) {
        return 6;
    }

    function priced(address) external pure returns (bool) {
        return true;
    }

    function movableAt(address) external pure returns (uint256) {}

    function heldValue(address, address[] calldata) external pure returns (uint256) {}
}

/// Lighter's exchange, as far as the keeper pays a station's pending
/// withdrawal out. Script keys: `pending`/owner.
contract StandInExchange is Scripted {
    error Nothing();

    function getPendingBalance(address owner, uint16) external view returns (uint128) {
        return uint128(_get("pending", _word(owner)));
    }

    function withdrawPendingBalance(address owner, uint16, uint128 baseAmount) external {
        uint256 pending = _get("pending", _word(owner));
        if (pending == 0 || baseAmount != pending) revert Nothing();
        _set("pending", _word(owner), 0);
    }
}

/// Lighter's board of posts, as far as the keeper reads the pools a
/// station's accounts hold shares in. Script keys: `poolCount`/station,
/// `pool`/station/i (a pool's index), `shares`/station/i.
contract StandInBoard is Scripted {
    function reportOf(address station) external view returns (LighterReport memory report, bool exists) {
        uint256 count = _get("poolCount", _word(station));
        report.pools = new LighterShares[](count);
        for (uint256 i = 0; i < count; i++) {
            report.pools[i].pool = uint48(_get("pool", _word(station), i));
            report.pools[i].shares = uint64(_get("shares", _word(station), i));
        }
        report.asOf = uint64(block.timestamp);
        exists = count != 0;
    }
}
