// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "../src/Vault.sol";
import {VaultFactory} from "../src/VaultFactory.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {IVault, VaultState} from "../src/interfaces/IVault.sol";
import {ChainName, VaultConfig} from "../src/interfaces/IVaultFactory.sol";
import {Fixture} from "./Fixture.sol";
import {MultiChainFactory} from "./MultiChainFactory.sol";

/// A vault still activating never opened, so it is cancelled, not killed: a
/// kill there would leave it neither activating (so it can't be cancelled)
/// nor able to close (its stations on other chains can only be valued by a
/// key that never checked in).
contract ActivatingVaultIsCancelledNotKilledTest is Fixture {
    Vault internal vault;

    function newFactory(
        VaultFactory.Deps memory deps,
        VaultConfig memory config,
        ChainName[] memory chains
    ) internal override returns (VaultFactory) {
        return new MultiChainFactory(deps, config, chains);
    }

    function setUp() public override {
        super.setUp();
        uint256 id = submitAs(author);
        bytes32 bundle = keccak256(abi.encode("bundle", id));
        ChainNeed[] memory chains = new ChainNeed[](2);
        chains[0].chain = "base";
        chains[0].drivers = new bytes32[](1);
        chains[0].drivers[0] = "local";
        chains[1].chain = "hyperliquid";
        chains[1].drivers = new bytes32[](1);
        chains[1].drivers[0] = "local";
        bytes32[] memory bridges = new bytes32[](1);
        bridges[0] = "local-bridge";
        bytes32[] memory restricted = new bytes32[](0);
        bytes32 digest = strategies.registrationDigest(
            id, author, keccak256("source"), bundle, keccak256("manifest"), chains, bridges, restricted
        );
        strategies.register(
            id,
            author,
            keccak256("source"),
            bundle,
            keccak256("manifest"),
            chains,
            bridges,
            restricted,
            signed(BUILDER_KEY, digest),
            signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")))
        );
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(bundle, 1));
        vault = activate(id, allowancesCosting(2e6), MINIMUM_TANK);
        assertTrue(vault.state() == VaultState.Activating);
    }

    function test_a_vault_still_activating_is_cancelled_not_killed() public {
        vm.prank(admin);
        vm.expectRevert(IVault.WrongState.selector);
        vault.kill();
        vm.prank(admin);
        vm.expectRevert(IVault.WrongState.selector);
        vault.windDownSealed();

        skip(3 days);
        vault.cancel();
        assertTrue(vault.state() == VaultState.Cancelled);
    }
}
