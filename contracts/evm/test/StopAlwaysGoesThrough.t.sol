// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {ILocalVenue, OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A halted vault's station can always be stopped: neither a venue that
/// refuses the revoke nor a driver whose views fault while preparing it can
/// keep the station from stopping, so anyone can then bring its money home.
contract StopAlwaysGoesThroughTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function deployed() internal {
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
    }

    /// Sound: the venue refusing the revoke itself does not stop the stop.
    function test_a_venue_that_refuses_the_revoke_cannot_keep_the_station_from_stopping() public {
        deployed();
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        vm.mockCallRevert(address(venue), abi.encodeWithSelector(ILocalVenue.revokeAgent.selector), "paused");
        station.stop();
        vm.clearMockedCalls();
        assertTrue(station.stopped());
        // The key whose revoke the venue refused stays on record, and anyone
        // may withdraw it once the venue takes the call.
        assertEq(station.approvedKey("local"), executor, "a refused revoke was recorded as done");
        vm.prank(bob);
        station.revokeKey("local", executor);
        assertEq(station.approvedKey("local"), address(0));
        assertFalse(venue.isAgent(address(station), executor));
        // And anyone can bring everything home.
        vm.prank(bob);
        station.unwind(1e18);
        assertEq(vault.cash(), 100_000e6 + DEAD_ASSETS);
    }

    /// Finding (low): a driver whose `revokeKey` *view* reverts makes `stop()`
    /// revert. The design says the stop never depends on the venue; here it
    /// depends on the driver's view being callable.
    function test_a_halted_station_can_still_be_stopped_when_the_drivers_revoke_view_reverts() public {
        deployed();
        // The venue pauses; its driver's `revokeKey` reads it and faults.
        RevokeFaultingDriver faulting = new RevokeFaultingDriver(venue, IERC20(address(usd)));
        pinnedDriverActsAs(address(faulting));
        skip(TIMELOCK);

        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);

        // A passer-by stops the station: this should not depend on a view.
        vm.prank(bob);
        (bool ok, bytes memory reason) = address(station).call(abi.encodeWithSignature("stop()"));
        assertTrue(ok, string(abi.encodePacked("stop() reverted: ", reason)));
        assertTrue(station.stopped(), "the station is not stopped");

        // Which is what lets anyone unwind it.
        vm.prank(bob);
        station.unwind(1e18);
        assertEq(vault.cash(), 100_000e6 + DEAD_ASSETS);
    }
}

/// The local driver, except that preparing a revoke reads the (paused) venue
/// and faults. Everything else works, so valuations, closes and sweeps all
/// go through it.
contract RevokeFaultingDriver is IDriver {
    error Paused();

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

    function revokeKey(address, address) external pure returns (Call[] memory) {
        revert Paused();
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
