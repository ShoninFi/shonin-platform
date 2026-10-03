// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {ILocalVenue, OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// Only the best-effort revoke at a stop is capped at a few calls; a key
/// whose honest revoke takes more is still withdrawn by anyone, strictly,
/// after a stop, and a blessed instance can still take its place.
contract LongRevokesStillGoThroughTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
        give(address(venue), 1_000_000e6);
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
        // The venue's honest revoke takes nine calls, all to the venue.
        pinnedDriverActsAs(address(new NineCallRevokeDriver(venue, IERC20(address(usd)))));
    }

    function test_a_key_whose_honest_revoke_takes_nine_calls_can_still_be_withdrawn_after_a_stop() public {
        deployed();
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        vm.prank(bob);
        station.stop();
        assertTrue(station.stopped());
        // Skipped, as the handbook says it may be ...
        assertEq(station.approvedKey("local"), executor, "the skipped revoke was recorded as done");

        // ... and "anyone can finish it strictly with revokeKey".
        vm.prank(bob);
        (bool ok, bytes memory reason) =
            address(station).call(abi.encodeCall(Station.revokeKey, ("local", executor)));
        assertTrue(ok, string(abi.encodePacked("revokeKey reverted: ", vm.toString(reason))));
        assertEq(venue.agentValidUntil(address(station), executor), 0, "the key is still an agent");
        assertEq(station.approvedKey("local"), address(0));
    }

    function test_a_blessed_instance_can_still_hand_over_from_a_key_whose_revoke_takes_nine_calls() public {
        deployed();
        address fresh = vm.addr(0xF4E5);
        blessExecutorKey(fresh, address(vault), bundle);
        vm.prank(fresh);
        (bool ok, bytes memory reason) =
            address(station).call(abi.encodeCall(Station.approveKey, ("local", fresh)));
        assertTrue(ok, string(abi.encodePacked("the handover reverted: ", vm.toString(reason))));
        assertEq(station.approvedKey("local"), fresh);
    }
}

/// The local driver, except that revoking a key honestly takes nine calls
/// to the venue (each one idempotent, as a per-sub-account revoke would be).
contract NineCallRevokeDriver is IDriver {
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

    function revokeKey(address station, address key) external view returns (Call[] memory calls) {
        Call memory one = inner.revokeKey(station, key)[0];
        calls = new Call[](9);
        for (uint256 i = 0; i < 9; i++) {
            calls[i] = one;
        }
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
