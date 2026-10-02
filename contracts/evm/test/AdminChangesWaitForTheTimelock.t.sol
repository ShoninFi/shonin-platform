// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";
import {TakingDriver} from "./invariant/VaultHandler.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// Whatever the admin key adds (a driver, a bridge, an image, a checker)
/// applies only once the registry's timelock has passed. Until then the
/// stations run what they ran, and a holder who doesn't want the change
/// leaves first, paid in full.
contract AdminChangesWaitForTheTimelockTest is ClaimBase {
    function setUp() public override {
        super.setUp();
        vm.prank(executor);
        station.approveKey("local", executor);
    }

    /// A replacement driver, proposed under the vault's own driver name,
    /// changes nothing while its timelock runs: the station still runs the
    /// driver it had.
    function test_a_proposed_driver_changes_nothing_before_its_timelock() public {
        depositAs(vault, alice, 100_000e6);
        deploySpot(90_000e6, 60_000e6, 0.5e18);
        TakingDriver replacement = new TakingDriver(IERC20(address(usd)), admin);
        vm.prank(admin);
        attestation.proposeDriver("local", address(replacement));

        skip(TIMELOCK - 1);
        blessExecutorKey(executor, address(vault), bundle);
        uint256 held = usd.balanceOf(address(station));
        vm.prank(executor);
        station.deposit("local", 1000e6);
        assertEq(usd.balanceOf(admin), 0, "the replacement applied inside the timelock");
        assertEq(usd.balanceOf(address(station)), held - 1000e6, "the deposit didn't go to the venue");
    }

    /// A holder who leaves as soon as a change is proposed is paid their full
    /// fraction before it applies.
    function test_a_holder_who_leaves_inside_the_timelock_is_paid_in_full() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        uint256 cash = vault.cash();
        deploySpot(cash * 94 / 100, cash * 94 / 100, (cash * 94 / 100) * 9 / 10 * 1e18 / PRICE);
        TakingDriver replacement = new TakingDriver(IERC20(address(usd)), admin);
        vm.prank(admin);
        attestation.proposeDriver("local", address(replacement));
        uint256 readyAt = block.timestamp + TIMELOCK;

        uint256 fair = worth(bobShares);
        uint256 id = claimFor(bob, bobShares);
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid, "the claim was not paid");
        assertLt(block.timestamp, readyAt, "it took longer than the timelock");
        // Less only the transaction fee and the profit fee the shares owe.
        assertGe(usd.balanceOf(bob), fair * 9980 / 10_000, "the leaver was paid short");
    }
}
