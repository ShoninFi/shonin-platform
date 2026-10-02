// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, Valuation, ValuationStatus, VaultState} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// Whether a halted vault's stations are empty is judged on the highest
/// figure any post carried: one post that sees another chain empty while
/// another still shows money there doesn't let the vault close.
contract CloseWaitsForEveryPostTest is TwoChainVault {
    function test_a_halted_vault_does_not_close_while_one_post_shows_money_elsewhere() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        uint256 bobShares = depositTwo(bob, 100_000e6);

        // Half the money goes to the other chain and is valued there.
        vm.startPrank(executor);
        station.pull(100_000e6);
        station.bridge("local-bridge", OTHER, 100_000e6, "");
        vm.stopPrank();
        valueWith(100_000e6, 100_000e6);

        vm.prank(admin);
        vault.kill();
        skip(1 hours);

        // The executor's post still sees the 100k on the other chain; the
        // unwinder's (read at a later block, or lagging) sees it empty.
        uint64 time = open();
        post(time, postOf(100_000e6, 100_000e6), EXECUTOR_KEY);
        post(time, postOf(0, 0), UNWINDER_KEY);
        closeAfterDeadline(time);
        Valuation memory v = latest();
        assertTrue(v.status == ValuationStatus.Priced);

        vm.expectRevert(IVault.NotYet.selector);
        vault.close();
        assertTrue(vault.state() != VaultState.Closed);

        // Once every post sees the other chain empty (its money home), the
        // vault closes, and equal shares are paid alike.
        give(address(station), 100_000e6);
        station.stop();
        station.unwind(1e18);
        valueWith(0, 0);
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);
        vm.prank(alice);
        uint256 alicePaid = vault.redeemClosed(aliceShares, alice);
        vm.prank(bob);
        uint256 bobPaid = vault.redeemClosed(bobShares, bob);
        assertApproxEqRel(alicePaid, bobPaid, 0.01e18, "equal shares were paid very differently");
    }
}
