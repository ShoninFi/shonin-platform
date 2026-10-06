// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {Station} from "../src/Station.sol";

import {Vault} from "../src/Vault.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {ILocalVenue, OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {IStation} from "../src/interfaces/IStation.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A best-effort revoke (at a stop, or a lapsed key's replacement) that
/// doesn't go through at the venue is recorded as not done: the station says
/// so, keeps a stopped station's key on record, and anyone can finish the
/// revoke strictly.
contract SkippedRevokeIsRecordedTest is Fixture {
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
    }

    /// The driver turns expensive: its `revokeKey` view is honest but costs
    /// more than the best-effort budget (a venue whose read grew).
    function expensiveDriver() internal {
        ExpensiveRevokeDriver expensive = new ExpensiveRevokeDriver(venue, IERC20(address(usd)));
        pinnedDriverActsAs(address(expensive));
    }

    function test_a_stop_whose_revoke_was_skipped_keeps_the_key_on_record_for_anyone_to_withdraw() public {
        deployed();
        expensiveDriver();
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);

        vm.expectEmit(address(station));
        emit IStation.RevokeSkipped("local", executor);
        vm.prank(bob);
        (bool ok,) = address(station).call{gas: 30_000_000}(abi.encodeWithSignature("stop()"));
        assertTrue(ok, "the stop reverted");
        assertTrue(station.stopped());
        assertEq(station.approvedKey("local"), executor, "a skipped revoke was recorded as done");

        // Anyone finishes it, strictly.
        vm.prank(bob);
        station.revokeKey("local", executor);
        assertFalse(venue.isAgent(address(station), executor));
        assertEq(station.approvedKey("local"), address(0));
    }

    function test_a_handover_whose_revoke_was_skipped_lets_anyone_withdraw_the_old_key() public {
        deployed();
        expensiveDriver();
        vm.prank(admin);
        attestation.revokeImage(EXECUTOR_IMAGE);
        bytes32 freshImage = keccak256("fresh executor image");
        vm.prank(admin);
        attestation.proposeImage(freshImage, ImageKind.Executor);
        skip(TIMELOCK);
        address fresh = vm.addr(0xF4E5);
        bytes memory journal = journalFor(freshImage, fresh, leafFor(address(vault), bundle, fresh));
        attestation.bless(
            sealOf(journal), journal, block.chainid, address(vault), bundle, fresh, new bytes32[](0)
        );

        vm.expectEmit(address(station));
        emit IStation.RevokeSkipped("local", executor);
        vm.prank(fresh);
        (bool ok,) =
            address(station).call{gas: 30_000_000}(abi.encodeCall(Station.approveKey, ("local", fresh)));
        assertTrue(ok, "the handover reverted");
        assertEq(station.approvedKey("local"), fresh);

        // The old key is unblessed, so anyone may withdraw it at the venue.
        vm.prank(bob);
        station.revokeKey("local", executor);
        assertFalse(venue.isAgent(address(station), executor));
        assertEq(station.approvedKey("local"), fresh);
    }
}

/// The local driver, except that preparing a revoke costs more than the
/// station's best-effort budget. It is honest: given the gas, it returns the
/// right call.
contract ExpensiveRevokeDriver is IDriver {
    uint256 private constant COST = 2_500_000;

    LocalDriver internal immutable inner;

    constructor(ILocalVenue venue_, IERC20 asset_) {
        inner = new LocalDriver(venue_, asset_);
    }

    function _spend() private view {
        uint256 start = gasleft();
        bytes32 x;
        while (start - gasleft() < COST) {
            x = keccak256(abi.encode(x));
        }
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

    function revokeKey(address station, address key) external view returns (Call[] memory) {
        _spend();
        return inner.revokeKey(station, key);
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
