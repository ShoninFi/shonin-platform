// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "../src/Vault.sol";
import {Tank} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A day's usage is charged only once the day is over, and never a day
/// before the vault opened, so a block of statements can't charge many days
/// at once and a charge never runs ahead of the time elapsed.
contract UsageCapIgnoresTimeTest is Fixture {
    function test_a_block_of_statements_cannot_charge_more_than_the_time_elapsed_allows() public {
        (Vault vault, bytes32 bundle) = openVault(); // tank $40, cap $2 a day
        uint64 opened = uint64(vm.getBlockTimestamp() / 1 days);
        skip(1 days);
        blessExecutorKey(executor, address(vault), bundle);
        uint256 tankBefore = vault.tank().balance;

        // The day it opened is over and can be charged, once.
        vault.submitUsage(opened, 2e6, signed(EXECUTOR_KEY, vault.usageDigest(opened, 2e6)));

        // Today and every later day are not over yet.
        for (uint64 i = 1; i <= 30; i++) {
            bytes memory statement = signed(EXECUTOR_KEY, vault.usageDigest(opened + i, 2e6));
            vm.expectRevert(Vault.DayNotOver.selector);
            vault.submitUsage(opened + i, 2e6, statement);
        }
        // Nor any day before it opened.
        bytes memory before = signed(EXECUTOR_KEY, vault.usageDigest(opened - 1, 2e6));
        vm.expectRevert(Vault.DayAlreadyCharged.selector);
        vault.submitUsage(opened - 1, 2e6, before);

        Tank memory tank = vault.tank();
        assertEq(tankBefore - tank.balance, 2e6);
        assertEq(tank.debt, 0);
    }
}
