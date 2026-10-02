// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, Request, ValuationStatus, VaultState} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// On a vault that trades on other chains, a request filed inside the post
/// window of the valuation that finds the stations empty doesn't hold off
/// the close, even once no key can value again: the vault closes, and the
/// request is settled at the closed vault's price.
contract StragglerSettledAfterTheCloseTest is TwoChainVault {
    function slot() internal returns (uint64 time) {
        uint256 t = vm.getBlockTimestamp();
        time = uint64(t - (t % SLOT) + SLOT);
        vm.warp(time);
    }

    /// Everything is cash at home; the vault is killed and its grace has
    /// passed; the executor opens the valuation that will show the stations
    /// empty. Returns that valuation's time, with the clock inside its window.
    function killedAndValuing() internal returns (uint64 time) {
        depositTwo(alice, 100_000e6);
        depositTwo(bob, 100_000e6);
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        time = open();
        assertTrue(latest().status == ValuationStatus.Open);
        vm.warp(time + 3);
    }

    /// Price it empty, then let the operator's keys lapse.
    function closedEmptyAndKeysGone(uint64 time) internal {
        post(time, postOf(0, 0), EXECUTOR_KEY);
        closeAfterDeadline(time);
        assertTrue(latest().status == ValuationStatus.Priced);
        assertEq(latest().gross, latest().cash + 1, "the stations were not found empty");

        // The blessings lapse (nobody re-attests a killed vault's machines).
        skip(BLESSING_TTL);
        slot();
        vm.prank(executor);
        vm.expectRevert(IVault.NotBlessed.selector);
        vault.recordValuation();
        vm.prank(unwinder);
        vm.expectRevert(IVault.NotBlessed.selector);
        vault.recordValuation();
    }

    function test_a_redeem_filed_in_the_empty_valuations_window_is_paid_after_the_close() public {
        uint64 time = killedAndValuing();

        // Inside the window the vault is not yet ready to close, so bob may
        // ask to leave; his request targets the next slot.
        uint256 bobShares = vault.balanceOf(bob);
        vm.prank(bob);
        uint256 id = vault.requestRedeem(bobShares, bob);
        Request memory r = vault.requestAt(id);
        assertGt(r.earliest, time);

        closedEmptyAndKeysGone(time);

        // The vault closes with bob's request outstanding...
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);
        // ...and bob is paid the closed vault's price, as alice is.
        uint256 bobPaid = vault.settleClosed(id);
        uint256 aliceShares = vault.balanceOf(alice);
        vm.prank(alice);
        uint256 alicePaid = vault.redeemClosed(aliceShares, alice);
        assertApproxEqRel(bobPaid * aliceShares, alicePaid * bobShares, 1e9, "one price for both");
        assertEq(usd.balanceOf(bob), bobPaid);
    }

    function test_a_strangers_keep_in_the_window_does_not_hold_off_the_close() public {
        // The author wants the tank kept topped up; it is below that mark.
        vm.prank(author);
        vault.setTankKeep(100e6);
        uint64 time = killedAndValuing();

        // A passer-by keeps the vault inside the window: a tank fill is
        // queued for the next slot.
        uint256 before = vault.requestCount();
        vm.prank(makeAddr("stranger"));
        vault.keep(0, 0, "");
        assertEq(vault.requestCount(), before + 1, "no tank fill was queued");

        closedEmptyAndKeysGone(time);

        vault.close();
        assertTrue(vault.state() == VaultState.Closed);
        // The fill lapses.
        vault.settleClosed(before + 1);
        assertTrue(vault.requestAt(before + 1).done);
    }
}
