// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {ILocalVenue, OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Fixture} from "./Fixture.sol";
import {RevokeFaultingDriver} from "./StopAlwaysGoesThrough.t.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A driver whose revoke names a target the station refuses, or whose
/// revoke view faults, can neither keep a halted station from stopping nor
/// keep a lapsed key from being replaced.
contract StopAndHandoverSurviveADriverTest is Fixture {
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

    function test_a_halted_station_can_still_be_stopped_when_the_drivers_revoke_names_a_bad_target() public {
        deployed();
        BadRevokeDriver bad = new BadRevokeDriver(venue, IERC20(address(usd)), alice);
        pinnedDriverActsAs(address(bad));
        skip(TIMELOCK);

        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);

        vm.prank(bob);
        (bool ok, bytes memory reason) = address(station).call(abi.encodeWithSignature("stop()"));
        assertTrue(ok, string(abi.encodePacked("stop() reverted: ", vm.toString(reason))));
        assertTrue(station.stopped(), "the station is not stopped");

        vm.prank(bob);
        station.unwind(1e18);
        assertEq(vault.cash(), 100_000e6 + DEAD_ASSETS);
    }

    /// The executor's blessing has lapsed; a fresh instance is blessed. The
    /// driver's `revokeKey` view faults (the venue is paused), so the new
    /// key can neither revoke the old one nor take its place.
    function test_a_lapsed_key_can_be_replaced_while_the_drivers_revoke_view_faults() public {
        deployed();
        RevokeFaultingDriver faulting = new RevokeFaultingDriver(venue, IERC20(address(usd)));
        pinnedDriverActsAs(address(faulting));
        skip(TIMELOCK);

        // The old key lapsed; a new instance is blessed.
        skip(BLESSING_TTL + 1);
        address fresh = vm.addr(0xF4E5);
        blessExecutorKey(fresh, address(vault), bundle);
        assertFalse(attestation.isBlessedFor(executor, block.chainid, address(vault), bundle));

        vm.prank(fresh);
        (bool ok,) = address(station).call(abi.encodeCall(Station.approveKey, ("local", fresh)));
        assertTrue(ok, "a blessed key could not replace a lapsed one");
        assertEq(station.approvedKey("local"), fresh);
    }
}

/// The local driver, except that its `revokeKey` answers with a call to an
/// address outside its targets, as a driver whose target list moved under it
/// would.
contract BadRevokeDriver is IDriver {
    LocalDriver internal immutable inner;
    address internal immutable elsewhere;

    constructor(ILocalVenue venue_, IERC20 asset_, address elsewhere_) {
        inner = new LocalDriver(venue_, asset_);
        elsewhere = elsewhere_;
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

    function revokeKey(address, address) external view returns (Call[] memory calls) {
        calls = new Call[](1);
        calls[0] = Call(elsewhere, 0, "");
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
