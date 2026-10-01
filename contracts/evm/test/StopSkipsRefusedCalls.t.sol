// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {ILocalVenue, OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A stop skips a call the station would refuse (a driver's revoke naming
/// a target it never declared) instead of reverting, so a halted vault's
/// station can always be stopped and its money brought home.
contract StopSkipsRefusedCallsTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function test_a_driver_whose_revoke_names_an_undeclared_target_cannot_keep_the_station_from_stopping()
        public
    {
        depositAs(vault, alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(90_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 90_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();

        // A later driver release prepares the revoke with a stray target.
        // The registry delivers it like any other, after the timelock.
        StrayRevokeDriver stray = new StrayRevokeDriver(venue, IERC20(address(usd)));
        vm.prank(admin);
        attestation.proposeDriver("local", address(stray));
        skip(TIMELOCK);

        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);

        vm.prank(bob);
        (bool ok, bytes memory reason) = address(station).call(abi.encodeWithSignature("stop()"));
        assertTrue(ok, string(abi.encodePacked("stop() reverted: ", vm.toString(reason))));
        assertTrue(station.stopped(), "the station is not stopped");

        // Which is what lets anyone bring everything home.
        vm.prank(bob);
        station.unwind(1e18);
        assertEq(vault.cash(), 100_000e6 + DEAD_ASSETS);
    }
}

/// The local driver, except that its revoke lists a target it never
/// declared. Everything else is the local driver's, so values, closes and
/// sweeps are untouched.
contract StrayRevokeDriver is IDriver {
    LocalDriver internal immutable inner;

    constructor(ILocalVenue venue_, IERC20 asset_) {
        inner = new LocalDriver(venue_, asset_);
    }

    function name() external pure returns (bytes32) {
        return "local";
    }

    function targets() external view returns (address[] memory) {
        return inner.targets();
    }

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

    function approveKey(address station, address key, uint64 until) external view returns (Call[] memory) {
        return inner.approveKey(station, key, until);
    }

    function revokeKey(address, address key) external pure returns (Call[] memory calls) {
        calls = new Call[](1);
        calls[0] = Call(address(0xBEEF), 0, abi.encodeCall(ILocalVenue.revokeAgent, (key)));
    }

    function deposit(address station, uint256 amount) external view returns (Call[] memory) {
        return inner.deposit(station, amount);
    }

    function withdraw(address station, uint256 amount) external view returns (Call[] memory) {
        return inner.withdraw(station, amount);
    }

    function close(
        address station,
        uint256 fraction,
        uint16 band
    ) external view returns (Call[] memory, bool) {
        return inner.close(station, fraction, band);
    }

    function sweep(address station, uint256 amount) external view returns (Call[] memory) {
        return inner.sweep(station, amount);
    }

    function valueOf(address station) external view returns (uint256, uint256) {
        return inner.valueOf(station);
    }
}
