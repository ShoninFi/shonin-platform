// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "../src/Vault.sol";
import {IVault, VaultState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A deposit sent back goes to whoever paid it, not to the receiver it named:
/// money returns where it came from.
contract RefundGoesToThePayerTest is Fixture {
    Vault internal vault;

    function setUp() public override {
        super.setUp();
        (vault,) = openVault();
    }

    function test_a_deposit_sent_back_goes_to_whoever_paid_it() public {
        bytes memory statement = statementBy(aliceKey, address(vault), alice, vault.restrictedText());
        give(bob, 1000e6);
        vm.startPrank(bob);
        usd.approve(address(vault), 1000e6);
        vault.requestDeposit(1000e6, alice, statement);
        vm.stopPrank();

        // The author retires before it is priced: deposits are sent back.
        vm.prank(author);
        vault.retire();
        value(vault);

        assertEq(usd.balanceOf(bob), 1000e6, "the payer was not refunded");
        assertEq(usd.balanceOf(alice), 0, "the receiver was paid the payer's money");
        assertEq(vault.balanceOf(alice), 0);
    }

    /// A deposit filed just before a kill can target a slot after the
    /// valuation that finds the stations empty; the vault then closes with it
    /// unpriced, and the closed vault's door sends it back to the payer too.
    function test_a_deposit_left_for_the_closed_vault_goes_back_to_whoever_paid_it() public {
        depositAs(vault, alice, 50_000e6);
        // Six seconds past a slot boundary: the deposit's slot is the one
        // after next, so the next slot's valuation does not price it.
        uint256 t = vm.getBlockTimestamp();
        uint256 boundary = t - (t % SLOT) + SLOT;
        vm.warp(boundary + 6);
        bytes memory statement = statementBy(aliceKey, address(vault), alice, vault.restrictedText());
        give(bob, 1000e6);
        vm.startPrank(bob);
        usd.approve(address(vault), 1000e6);
        uint256 id = vault.requestDeposit(1000e6, alice, statement);
        vm.stopPrank();
        vm.prank(admin);
        vault.kill();

        value(vault);
        assertGt(vault.requestAt(id).earliest, vault.valuationAt(vault.valuationCount() - 1).time);
        assertFalse(vault.requestAt(id).done, "the valuation priced the deposit");
        vault.close();
        assertTrue(vault.state() == VaultState.Closed, "the vault did not close");
        assertFalse(vault.requestAt(id).done, "the close settled the deposit");

        uint256 aliceBefore = usd.balanceOf(alice);
        uint256 escrowBefore = vault.escrow();
        vm.prank(makeAddr("anyone"));
        vault.settleClosed(id);
        assertTrue(vault.requestAt(id).done);
        assertEq(usd.balanceOf(bob), 1000e6, "the payer was not refunded");
        assertEq(usd.balanceOf(alice), aliceBefore, "the receiver was paid the payer's money");
        assertEq(escrowBefore - vault.escrow(), 1000e6, "the escrow did not give up the deposit");
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.settleClosed(id);
    }

    function test_a_depositor_paying_for_themselves_is_refunded_as_before() public {
        requestDepositAs(vault, alice, 1000e6);
        vm.prank(author);
        vault.retire();
        value(vault);
        assertEq(usd.balanceOf(alice), 1000e6);
    }
}
