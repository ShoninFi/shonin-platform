// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Closer} from "../src/Closer.sol";
import {VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A halted vault is stopped, emptied, valued and closed in one transaction,
/// so nobody can get between the steps: a stranger who sends the stopped
/// station a little money before each valuation can't keep it from ever
/// reading as empty.
contract HaltedVaultClosesInOneTransactionTest is ClaimBase {
    Closer internal closer;
    address internal stranger = makeAddr("stranger");

    function setUp() public override {
        super.setUp();
        closer = new Closer();
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 50_000e6);
        deploySpot(120_000e6, 100_000e6, 2e18);
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
    }

    function atSlot() internal {
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
    }

    function test_a_halted_vault_closes_in_one_call() public {
        atSlot();
        closer.closeHalted(address(vault));
        assertTrue(vault.state() == VaultState.Closed);
        assertEq(spotSize(), 0);
        (uint256 mark,) = station.value();
        assertEq(mark, 0);
    }

    function test_a_gift_to_the_stopped_station_does_not_hold_the_close() public {
        // Emptied the slow way, then a gift lands on the station before the
        // next valuation: valued on its own, the station isn't empty.
        station.stop();
        station.unwind(1e18);
        give(stranger, 5e6);
        vm.prank(stranger);
        usd.transfer(address(station), 5e6);
        (uint256 mark,) = station.value();
        assertGt(mark, 0);

        // In one call the gift comes home with everything else, and the
        // vault closes.
        atSlot();
        closer.closeHalted(address(vault));
        assertTrue(vault.state() == VaultState.Closed);
    }

    function test_a_valuation_already_recorded_this_slot_does_not_hold_the_close() public {
        // Emptied the slow way, and somebody values the empty station first.
        station.stop();
        station.unwind(1e18);
        atSlot();
        vm.prank(stranger);
        vault.recordValuation();

        // The close is judged on that valuation instead of recording another.
        closer.closeHalted(address(vault));
        assertTrue(vault.state() == VaultState.Closed);
    }

    /// What one transaction can't prevent: valuations are open to anyone and
    /// there is one a slot, so a stranger who gifts the emptied station more
    /// than dust and records the slot's valuation before the close holds the
    /// close off for that slot. It costs them the gift, which comes home with
    /// the next slot's close and stays with the holders, and it buys one slot.
    function test_a_gift_and_a_valuation_sent_first_hold_the_close_off_one_slot_at_the_cost_of_the_gift()
        public
    {
        station.stop();
        station.unwind(1e18);
        uint256 gift = MINIMUM_DEPOSIT / 10 + 1;

        atSlot();
        give(stranger, gift);
        vm.startPrank(stranger);
        usd.transfer(address(station), gift);
        vault.recordValuation();
        vm.stopPrank();

        vm.expectRevert();
        closer.closeHalted(address(vault));
        assertFalse(vault.state() == VaultState.Closed);

        // The next slot the gift is brought home with everything else, and
        // the vault closes with it in the holders' cash.
        uint256 held = usd.balanceOf(address(vault));
        atSlot();
        closer.closeHalted(address(vault));
        assertTrue(vault.state() == VaultState.Closed);
        assertEq(usd.balanceOf(address(vault)), held + gift);
        assertEq(usd.balanceOf(stranger), 0);
    }

    function test_outside_the_slot_it_changes_nothing() public {
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT + 5);
        uint256 size = spotSize();
        vm.expectRevert();
        closer.closeHalted(address(vault));
        assertEq(spotSize(), size);
        assertFalse(station.stopped());
    }
}
