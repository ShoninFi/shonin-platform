// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {IVault, VaultState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// Once a halted vault's station has been found empty, it takes no new
/// request of any kind: no redeem, no top-up from the author's shares, no
/// fill queued by `keep`. Nothing can hold its close off a slot at a time.
contract CloseCannotBeStalledByTheTankTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;
    uint256 internal aliceShares;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    /// A killed vault, its station stopped, unwound, and found empty by a
    /// valuation strictly after the halt: ready to close.
    function readyToClose() internal {
        aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 10e6);
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);
        value(vault);
        assertTrue(vault.state() == VaultState.Killed);
        // Alice is already refused a redeem request, by design: she is meant
        // to leave through the close.
        vm.prank(alice);
        vm.expectRevert(IVault.WrongState.selector);
        vault.requestRedeem(aliceShares, alice);
    }

    /// The closer does the best it can: in one transaction, at a slot
    /// boundary, record the valuation, settle what it prices, and close.
    function tryToClose() internal returns (bool closed) {
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        vault.settle(type(uint256).max);
        (closed,) = address(vault).call(abi.encodeCall(vault.close, ()));
    }

    function test_the_author_cannot_top_up_a_vault_ready_to_close() public {
        readyToClose();
        vm.prank(author);
        vm.expectRevert(IVault.WrongState.selector);
        vault.topUpFromShares(1);
        assertTrue(tryToClose(), "the vault did not close");
    }

    function test_a_keep_queues_no_fill_in_a_vault_ready_to_close() public {
        vm.prank(author);
        vault.setTankKeep(1000e6);
        readyToClose();
        uint256 requests = vault.requestCount();
        vm.prank(bob);
        vault.keep(0, 0, "");
        assertEq(vault.requestCount(), requests, "a keep queued a fill");
        assertTrue(tryToClose(), "the vault did not close");
    }
}
