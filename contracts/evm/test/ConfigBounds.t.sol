// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "../src/Vault.sol";
import {VaultFactory} from "../src/VaultFactory.sol";
import {VaultConfig} from "../src/interfaces/IVaultFactory.sol";
import {Fixture} from "./Fixture.sol";

/// The factory's terms have ceilings, checked at deployment and on every
/// change, so no config can make a fee take what it was never described as
/// taking: at a 100% transaction fee a deposit would mint nothing and the
/// money would stay in the pool. And an author is held to the terms they
/// read: a change between reading and activating stops the activation.
contract ConfigBoundsTest is Fixture {
    function refused(VaultConfig memory config) internal {
        vm.prank(admin);
        vm.expectRevert(VaultFactory.OutOfRange.selector);
        factory.setConfig(config);
    }

    function test_a_fee_that_takes_everything_is_out_of_range() public {
        VaultConfig memory config = factory.config();
        config.transactionBps = 10_000;
        refused(config);

        config = factory.config();
        config.holdingBps = uint16(factory.MAX_HOLDING_BPS() + 1);
        refused(config);

        config = factory.config();
        config.authorBps = 4000;
        config.platformBps = 1001;
        refused(config);

        config = factory.config();
        config.exitBandBps = uint16(factory.MAX_EXIT_BAND_BPS() + 1);
        refused(config);
    }

    function test_the_timers_and_amounts_must_make_sense() public {
        VaultConfig memory config = factory.config();
        config.timers.freezeAfter = 0;
        refused(config);

        config = factory.config();
        config.deadAssets = 0;
        refused(config);

        config = factory.config();
        config.minimumTank = config.deadAssets;
        refused(config);

        config = factory.config();
        config.timers.slotWindow = config.timers.slotInterval;
        refused(config);

        config = factory.config();
        config.minimumDeposit = 0;
        refused(config);
    }

    function test_an_activation_is_on_the_terms_the_author_read() public {
        (uint256 id,) = ready();
        bytes32 read = factory.terms();

        VaultConfig memory changed = factory.config();
        changed.transactionBps = uint16(factory.MAX_TRANSACTION_BPS());
        vm.prank(admin);
        factory.setConfig(changed);

        (, string memory text) = factory.restrictedFor(id);
        bytes memory statement = statementBy(authorKey, factory.predictVault(id), author, text);
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        vm.expectRevert(VaultFactory.TermsChanged.selector);
        factory.activate(id, allowancesCosting(2e6), 500, MINIMUM_TANK, 0, statement, read);
        vm.stopPrank();
    }

    /// Even at the ceiling, a deposit buys shares.
    function test_a_vault_on_the_harshest_terms_allowed_still_gives_a_depositor_shares() public {
        VaultConfig memory config = factory.config();
        config.transactionBps = uint16(factory.MAX_TRANSACTION_BPS());
        vm.prank(admin);
        factory.setConfig(config);

        (Vault vault,) = openVault();
        assertEq(vault.fees().transactionBps, factory.MAX_TRANSACTION_BPS());
        assertGt(depositAs(vault, alice, 1000e6), 0);
    }
}
