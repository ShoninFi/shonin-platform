// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "../src/Vault.sol";
import {Request} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// Requests taken through the fallback in any order never hold the queue:
/// `settle` passes requests already done at the cursor whatever their slot,
/// so a bank run of fallbacks taken out of order leaves the vault able to
/// value, pay its next leaver and close.
contract FallbacksInAnyOrderNeverHoldTheQueueTest is Fixture {
    uint256 internal constant N = 130;

    Vault internal vault;

    function setUp() public override {
        super.setUp();
        (vault,) = openVault();
    }

    function test_fallbacks_taken_in_reverse_order_leave_valuations_open() public {
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100e6);

        // Bob (or N different leavers in a bank run) asks to redeem.
        vm.startPrank(bob);
        uint256 first = vault.requestRedeem(1, bob);
        for (uint256 i = 1; i < N; i++) {
            vault.requestRedeem(1, bob);
        }
        vm.stopPrank();

        // A day passes with nothing priced (a wide market, a keeper outage).
        Request memory r = vault.requestAt(first);
        vm.warp(r.earliest + 24 hours + 1);

        // The fallback is taken in reverse order: only the last take moves
        // the cursor, by 64, leaving 66 done requests in front of it.
        vm.startPrank(bob);
        for (uint256 i = N; i > 0; i--) {
            vault.takeProRata(first + i - 1);
        }
        vm.stopPrank();

        // Alice now wants out.
        uint256 aliceShares = vault.balanceOf(alice);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);

        // The keeper settles as much as it likes before it values, as the
        // build plan says it does.
        for (uint256 k = 0; k < 10; k++) {
            vault.settle(64);
        }
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        vault.settle(type(uint256).max);

        assertGt(usd.balanceOf(alice), 99_000e6, "alice was never paid");
    }

    /// After the same run of fallbacks, a killed vault is valued and closes.
    function test_a_killed_vault_still_closes_after_them() public {
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100e6);
        vm.startPrank(bob);
        uint256 first = vault.requestRedeem(1, bob);
        for (uint256 i = 1; i < N; i++) {
            vault.requestRedeem(1, bob);
        }
        vm.stopPrank();
        vm.warp(vault.requestAt(first).earliest + 24 hours + 1);
        vm.startPrank(bob);
        for (uint256 i = N; i > 0; i--) {
            vault.takeProRata(first + i - 1);
        }
        vm.stopPrank();

        vm.prank(admin);
        vault.kill();
        vm.warp(vm.getBlockTimestamp() + 3 days);
        for (uint256 k = 0; k < 10; k++) {
            vault.settle(64);
        }
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        vault.close();
    }
}
