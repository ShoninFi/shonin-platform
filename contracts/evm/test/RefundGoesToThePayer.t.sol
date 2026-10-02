// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "../src/Vault.sol";
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

    function test_a_depositor_paying_for_themselves_is_refunded_as_before() public {
        requestDepositAs(vault, alice, 1000e6);
        vm.prank(author);
        vault.retire();
        value(vault);
        assertEq(usd.balanceOf(alice), 1000e6);
    }
}
