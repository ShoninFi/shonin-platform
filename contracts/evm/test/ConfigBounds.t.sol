// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "../src/Vault.sol";
import {VaultFactory} from "../src/VaultFactory.sol";
import {VaultConfig} from "../src/interfaces/IVaultFactory.sol";
import {Fixture} from "./Fixture.sol";

/// The factory's terms have ceilings, checked at deployment and on every
/// change, so no config can make a fee take what it was never described as
/// taking: at a 100% transaction fee a deposit would mint nothing and the
/// money would stay in the pool.
contract ConfigBoundsTest is Fixture {
    function test_a_fee_that_takes_everything_is_out_of_range() public {
        VaultConfig memory config = factory.config();
        config.transactionBps = 10_000;
        vm.prank(admin);
        vm.expectRevert(VaultFactory.OutOfRange.selector);
        factory.setConfig(config);

        config = factory.config();
        config.streamingBps = uint16(factory.MAX_HOLDING_BPS() + 1);
        vm.prank(admin);
        vm.expectRevert(VaultFactory.OutOfRange.selector);
        factory.setConfig(config);

        config = factory.config();
        config.freezeAfter = 0;
        vm.prank(admin);
        vm.expectRevert(VaultFactory.OutOfRange.selector);
        factory.setConfig(config);

        config = factory.config();
        config.minimumTank = 0;
        vm.prank(admin);
        vm.expectRevert(VaultFactory.OutOfRange.selector);
        factory.setConfig(config);
    }

    /// Even at the ceiling, a deposit buys shares.
    function test_a_vault_on_the_harshest_terms_allowed_still_gives_a_depositor_shares() public {
        VaultConfig memory config = factory.config();
        config.transactionBps = uint16(factory.MAX_TRANSACTION_BPS());
        vm.prank(admin);
        factory.setConfig(config);

        (uint256 id,) = ready();
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        Vault vault = Vault(factory.activate(id, allowancesCosting(2e6), MINIMUM_TANK));
        vm.stopPrank();

        assertEq(vault.fees().transactionBps, factory.MAX_TRANSACTION_BPS());
        assertGt(depositAs(vault, alice, 1000e6), 0);
    }
}
