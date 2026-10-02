// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {ILocalVenue, OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// The station's self-calls (`revokeOnce`, `closeAll`) answer only the
/// station, and a venue reached from inside one can't re-enter it.
contract SelfCallsAreSelfOnlyTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function test_nobody_but_the_station_can_call_revokeOnce_or_closeAll() public {
        vm.prank(bob);
        vm.expectRevert(Station.NotAllowed.selector);
        station.revokeOnce("local", executor);

        vm.prank(executor);
        vm.expectRevert(Station.NotAllowed.selector);
        station.revokeOnce("local", executor);

        vm.prank(bob);
        vm.expectRevert(Station.NotAllowed.selector);
        station.closeAll("local", 100);

        vm.prank(address(vault));
        vm.expectRevert(Station.NotAllowed.selector);
        station.closeAll("local", 100);
    }

    function test_a_venue_reached_from_a_best_effort_revoke_cannot_reenter_the_station() public {
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

        // The driver's revoke goes through a contract that tries every
        // guarded entry point while the station is mid-revoke.
        Reenterer reenterer = new Reenterer(station);
        pinnedDriverActsAs(
            address(new ReenteringRevokeDriver(venue, IERC20(address(usd)), address(reenterer)))
        );

        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        vm.prank(bob);
        station.stop();
        assertTrue(station.stopped());

        assertTrue(reenterer.reached(), "the reenterer was never called");
        assertEq(reenterer.successes(), 0, "a guarded station entry point was re-entered from a revoke");
        // The honest part of the revoke still went through.
        assertEq(venue.agentValidUntil(address(station), executor), 0, "the key was not revoked");
        assertEq(station.approvedKey("local"), address(0));
    }
}

/// Called from inside a revoke: tries to re-enter the station every way a
/// stranger could, and counts what got through.
contract Reenterer {
    Station internal immutable station;
    bool public reached;
    uint256 public successes;

    constructor(Station station_) {
        station = station_;
    }

    function poke() external {
        reached = true;
        bytes32[] memory none = new bytes32[](0);
        _try(abi.encodeWithSignature("stop()"));
        _try(abi.encodeCall(Station.revokeKey, ("local", address(0xBEEF))));
        _try(abi.encodeCall(Station.approveKey, ("local", address(0xBEEF))));
        _try(abi.encodeCall(Station.unwind, (1e18)));
        _try(abi.encodeCall(Station.unwindStopped, (none)));
        _try(abi.encodeCall(Station.push, (1)));
    }

    function _try(bytes memory data) private {
        (bool ok,) = address(station).call(data);
        if (ok) successes++;
    }
}

/// The local driver, except that its revoke also calls the reenterer, which
/// it lists among its targets.
contract ReenteringRevokeDriver is IDriver {
    LocalDriver internal immutable inner;
    address internal immutable reenterer;

    constructor(ILocalVenue venue_, IERC20 asset_, address reenterer_) {
        inner = new LocalDriver(venue_, asset_);
        reenterer = reenterer_;
    }

    function name() external pure returns (bytes32) {
        return "local";
    }

    function targets() external view returns (address[] memory list) {
        list = new address[](2);
        list[0] = inner.targets()[0];
        list[1] = reenterer;
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

    function revokeKey(address station, address key) external view returns (Call[] memory calls) {
        calls = new Call[](2);
        calls[0] = Call(reenterer, 0, abi.encodeCall(Reenterer.poke, ()));
        calls[1] = inner.revokeKey(station, key)[0];
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
