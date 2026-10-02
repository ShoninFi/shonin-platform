// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {IVault} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A driver whose reads fault stops every valuation until the registry's
/// timelock delivers a fixed one. Meanwhile no money that never left the
/// vault is stuck: a deposit nothing has priced for `fallbackAfter` goes back,
/// and a holder takes the cash's share through the pro-rata fallback.
contract FaultingDriverEscapeTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function test_cash_that_never_left_the_vault_can_still_be_paid_out() public {
        depositAs(vault, alice, 1000e6);
        uint256 id = requestDepositAs(vault, bob, 500e6);

        // The venue's contract starts reverting on reads: the driver that
        // reaches it faults.
        FaultingDriver faulting = new FaultingDriver();
        pinnedDriverActsAs(address(faulting));
        skip(TIMELOCK);

        // No valuation can be recorded, so bob's deposit stays in escrow.
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vm.expectRevert();
        vault.recordValuation();
        assertFalse(vault.requestAt(id).done);
        assertEq(vault.escrow(), 500e6);

        // A deposit nothing has priced for a day goes back: its money never
        // left the vault.
        skip(1 days);
        vm.prank(bob);
        vault.takeProRata(id);
        assertEq(usd.balanceOf(bob), 500e6, "bob's escrowed deposit came back");
        assertEq(vault.escrow(), 0);

        // A holder takes the cash's share through the same fallback.
        uint256 aliceShares = vault.balanceOf(alice);
        vm.prank(alice);
        uint256 leaving = vault.requestRedeem(aliceShares, alice);
        skip(1 days + SLOT);
        vm.prank(alice);
        vault.takeProRata(leaving);
        assertGt(usd.balanceOf(alice), 990e6, "alice took her share of the cash");
    }
}

/// A driver whose reads revert, as a paused venue's would.
contract FaultingDriver is IDriver {
    error Paused();

    function name() external pure returns (bytes32) {
        return "local";
    }

    function targets() external pure returns (address[] memory) {
        return new address[](0);
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

    function approveKey(address, address, uint64) external pure returns (Call[] memory) {
        revert Paused();
    }

    function revokeKey(address, address) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function deposit(address, uint256) external pure returns (Call[] memory) {
        revert Paused();
    }

    function withdraw(address, uint256) external pure returns (Call[] memory) {
        revert Paused();
    }

    function close(address, uint256, uint16) external pure returns (Call[] memory, bool) {
        revert Paused();
    }

    function sweep(address, uint256) external pure returns (Call[] memory) {
        revert Paused();
    }

    function valueOf(address) external pure returns (uint256, uint256) {
        revert Paused();
    }
}
