// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {VaultState} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// On a vault that trades on another chain too, what the other station holds
/// is known only from the posts a valuation gathers: whether the close wants
/// a valuation is judged on the home station, and the posts decide.
contract CloseWantsValuationOnTwoChainsTest is TwoChainVault {
    function nextSlot() internal view returns (uint64) {
        uint256 t = vm.getBlockTimestamp();
        return uint64(t - (t % SLOT) + SLOT);
    }

    function test_close_wants_valuation_on_two_chains_judges_the_home_station_and_the_posts_decide() public {
        depositTwo(alice, 100_000e6);
        depositTwo(bob, 100_000e6);
        // Half of it goes to the other chain and is valued there.
        vm.startPrank(executor);
        station.pull(100_000e6);
        station.bridge("local-bridge", OTHER, 100_000e6, "");
        vm.stopPrank();
        valueWith(100_000e6, 100_000e6);
        vm.prank(admin);
        vault.kill();
        skip(1 hours);

        // The home station holds nothing.
        assertTrue(vault.closeWantsValuation(nextSlot()));

        // Not while a valuation waits for its posts.
        uint64 time = open();
        assertFalse(vault.closeWantsValuation(time + SLOT), "wanted while one is open");
        // The post still shows the money on the other chain: the vault can't
        // close, and the home station alone still says a valuation could.
        post(time, postOf(100_000e6, 100_000e6), EXECUTOR_KEY);
        closeAfterDeadline(time);
        assertFalse(vault.readyToClose());
        assertTrue(vault.closeWantsValuation(nextSlot()));

        // Once a post finds it empty the vault is ready, and wants no more.
        valueWith(0, 0);
        assertTrue(vault.readyToClose());
        assertFalse(vault.closeWantsValuation(nextSlot()));
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);
    }
}
