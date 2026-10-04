// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// `close()` doesn't wait for the halt's grace. A vault whose stations hold
/// nothing (a strategy holding cash, or one that wound down to cash early in
/// its grace) is valued empty and closed by anyone inside the grace, and its
/// author may then take the whole tank at once. That is why the keeper
/// collects the bill from the moment a vault halts, not once the grace is
/// over.
contract HaltedVaultMayCloseInsideItsGraceTest is ClaimBase {
    function setUp() public override {
        super.setUp();
        depositAs(vault, alice, 100_000e6);
    }

    function test_a_halted_vault_closes_and_its_tank_can_be_taken_inside_the_grace() public {
        uint256 tank = vault.tank().balance;
        vm.prank(admin);
        vault.kill();
        uint64 haltedAt = vault.haltedAt();
        skip(60);
        // Any valuation after the halt that finds the station empty.
        value(vault);
        assertLt(block.timestamp, haltedAt + vault.haltGrace(), "still inside the grace");
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);

        uint256 before = usd.balanceOf(author);
        vm.prank(author);
        vault.withdrawTankUnsigned(type(uint256).max);
        assertGe(usd.balanceOf(author) - before, tank, "the author took the tank at once");
        assertEq(vault.tank().balance, 0);
    }
}
