// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Call, IDriver} from "../../src/interfaces/IDriver.sol";
import {StationSpec} from "../../src/interfaces/IStation.sol";

/// Stands where a station stands for a driver: it owns the venue accounts,
/// answers `spec` and `approvedKey`, and makes each call a driver prepares
/// after the same check the station makes (a target the driver declares, or
/// the dollar only to approve one). Used where the whole vault can't run,
/// such as on a fork of a chain whose dollar the fixture can't mint.
contract StationDouble {
    bytes4 private constant APPROVE = 0x095ea7b3;

    address public immutable dollar;
    uint16 public band;
    mapping(bytes32 => address) public approvedKey;

    error BadCall();

    constructor(address dollar_, uint16 band_) {
        dollar = dollar_;
        band = band_;
    }

    function spec() external view returns (StationSpec memory s) {
        s.vaultChain = block.chainid;
        s.exitBandBps = band;
    }

    function setApprovedKey(bytes32 driver, address key) external {
        approvedKey[driver] = key;
    }

    /// Make `calls`, checked against `driver`'s targets as the station checks
    /// them.
    function run(IDriver driver, Call[] memory calls) public {
        address[] memory targets = driver.targets();
        for (uint256 i = 0; i < calls.length; i++) {
            Call memory c = calls[i];
            bool allowed;
            if (c.target == dollar) {
                if (c.data.length == 68 && bytes4(c.data) == APPROVE) {
                    address spender;
                    bytes memory data = c.data;
                    assembly {
                        spender := mload(add(data, 36))
                    }
                    allowed = _includes(targets, spender);
                }
            } else {
                allowed = _includes(targets, c.target);
            }
            if (!allowed) revert BadCall();
            (bool ok, bytes memory reason) = c.target.call{value: c.value}(c.data);
            if (!ok) {
                assembly {
                    revert(add(reason, 32), mload(reason))
                }
            }
        }
    }

    function approveKey(IDriver driver, address key) external {
        run(driver, driver.approveKey(address(this), key, type(uint64).max));
        approvedKey[driver.name()] = key;
    }

    function revokeKey(IDriver driver, address key) external {
        run(driver, driver.revokeKey(address(this), key));
    }

    function deposit(IDriver driver, uint256 amount) external {
        run(driver, driver.deposit(address(this), amount));
    }

    function withdraw(IDriver driver, uint256 amount) external {
        run(driver, driver.withdraw(address(this), amount));
    }

    /// A leaver's close as the station makes it: blocked closes nothing.
    function close(IDriver driver, uint256 fraction) external returns (bool blocked) {
        Call[] memory calls;
        (calls, blocked) = driver.close(address(this), fraction, band);
        if (blocked) return true;
        run(driver, calls);
    }

    function sweep(IDriver driver, uint256 amount) external {
        run(driver, driver.sweep(address(this), amount));
    }

    function _includes(address[] memory list, address item) private pure returns (bool) {
        for (uint256 i = 0; i < list.length; i++) {
            if (list[i] == item) return true;
        }
        return false;
    }
}
