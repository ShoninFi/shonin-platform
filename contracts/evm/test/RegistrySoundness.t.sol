// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {StrategyRegistry} from "../src/StrategyRegistry.sol";
import {Vault} from "../src/Vault.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Allowances, UnitPrices} from "../src/interfaces/IPrices.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {VaultConfig} from "../src/interfaces/IVaultFactory.sol";
import {Fixture} from "./Fixture.sol";

/// Properties of the registries, the prices and the factory that were
/// checked and found to hold, kept as regression tests.
contract RegistrySoundnessTest is Fixture {
    // AttestationRegistry ---------------------------------------------------

    /// A station on this chain serving a vault that lives on another chain
    /// asks by the vault's chain. The leaf carries both chains, so the
    /// blessing answers for (vaultChain, vault) here and for nothing else.
    function test_a_key_is_blessed_for_a_vault_on_another_chain_by_that_chain() public {
        uint256 vaultChain = 8453;
        address vault = makeAddr("vault on base");
        bytes32 bundle = keccak256("bundle");
        bytes32 leaf =
            keccak256(bytes.concat(keccak256(abi.encode(vaultChain, vault, bundle, block.chainid))));
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, leaf);
        attestation.bless(sealOf(journal), journal, vaultChain, vault, bundle, new bytes32[](0));

        assertTrue(attestation.isBlessedFor(executor, vaultChain, vault, bundle));
        assertFalse(attestation.isBlessedFor(executor, block.chainid, vault, bundle));
        assertEq(attestation.keyInfo(executor).vaultChain, vaultChain);

        // The same seal on another chain id proves nothing: the leaf named this one.
        bytes memory seal = sealOf(journal);
        vm.chainId(block.chainid + 1);
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.bless(seal, journal, vaultChain, vault, bundle, new bytes32[](0));
    }

    /// Two lowerings in a row: the second discards the first and waits the
    /// timelock in force when it was asked for, so the first never lands early.
    function test_a_second_lowering_replaces_the_first_and_waits_the_full_current_timelock() public {
        uint64 t0 = uint64(vm.getBlockTimestamp());
        vm.prank(admin);
        attestation.setTimelock(1 hours); // effective t0 + 2 days
        skip(1 days);
        vm.prank(admin);
        attestation.setTimelock(0); // effective t0 + 1 day + 2 days
        skip(1 days); // t0 + 2 days: the first lowering would have landed now
        assertEq(attestation.timelock(), TIMELOCK, "the discarded lowering applied");
        (uint64 pending, uint64 at) = attestation.pendingTimelock();
        assertEq(pending, 0);
        assertEq(at, t0 + 3 days);
        skip(1 days);
        assertEq(attestation.timelock(), 0);
    }

    /// A key is bound to one image and one project for life; a seal from a
    /// different allowed project naming the same key is refused, so no second
    /// operator can claim a running key.
    function test_a_key_cannot_be_claimed_by_another_project() public {
        bytes32 otherProject = keccak256("another operator");
        vm.prank(admin);
        attestation.proposeProject(otherProject);
        skip(TIMELOCK);
        bytes memory journal = abi.encode(
            AttestationRegistry.Journal({
                imageDigest: BUILDER_IMAGE,
                projectId: otherProject,
                signer: builder,
                vaultsRoot: serviceLeaf(),
                issuedAt: uint64(vm.getBlockTimestamp())
            })
        );
        bytes memory seal = sealOf(journal);
        vm.expectRevert(AttestationRegistry.NotThisInstance.selector);
        attestation.bless(seal, journal, 0, address(0), bytes32(0), new bytes32[](0));
    }

    // StrategyRegistry --------------------------------------------------------

    /// The two signatures a registration needs are over distinct, purposed
    /// digests: a reviewer's clearance never passes as a builder's signature
    /// over the same strategy, nor the reverse, even when the same key holds
    /// both kinds' worth of blessings elsewhere.
    function test_neither_signature_serves_the_other_purpose() public {
        uint256 id = submitAs(author);
        ChainNeed[] memory chains = oneChain();
        bytes32[] memory none = new bytes32[](0);
        bytes32 regDigest = strategies.registrationDigest(
            id, author, keccak256("source"), keccak256("bundle"), keccak256("manifest"), chains, none, none
        );
        bytes32 clrDigest = strategies.clearanceDigest(id, keccak256("source"));
        assertTrue(regDigest != clrDigest);

        // The reviewer signs the registration digest: a signature from a
        // non-builder, however blessed.
        bytes memory reviewerOverReg = signed(REVIEWER_KEY, regDigest);
        bytes memory clearance = signed(REVIEWER_KEY, clrDigest);
        vm.expectRevert(StrategyRegistry.NotTheBuilder.selector);
        strategies.register(
            id,
            author,
            keccak256("source"),
            keccak256("bundle"),
            keccak256("manifest"),
            chains,
            none,
            none,
            reviewerOverReg,
            clearance
        );

        // The builder signs the clearance digest: not a reviewer.
        bytes memory builderSig = signed(BUILDER_KEY, regDigest);
        bytes memory builderOverClr = signed(BUILDER_KEY, clrDigest);
        vm.expectRevert(StrategyRegistry.NotCleared.selector);
        strategies.register(
            id,
            author,
            keccak256("source"),
            keccak256("bundle"),
            keccak256("manifest"),
            chains,
            none,
            none,
            builderSig,
            builderOverClr
        );
        assertFalse(strategies.isRegistered(id));
    }

    /// A registration signed for one registry does not register on another
    /// deployed on the same chain, and one signed on one chain does not
    /// register on another chain.
    function test_registration_signatures_are_bound_to_this_registry_and_this_chain() public {
        uint256 id = submitAs(author);
        StrategyRegistry twin = new StrategyRegistry(attestation, publishing);
        ChainNeed[] memory chains = oneChain();
        bytes32[] memory none = new bytes32[](0);
        bytes32 digestForTwin = twin.registrationDigest(
            id, author, keccak256("source"), keccak256("bundle"), keccak256("manifest"), chains, none, none
        );
        bytes memory builderSig = signed(BUILDER_KEY, digestForTwin);
        bytes memory clearance = signed(REVIEWER_KEY, twin.clearanceDigest(id, keccak256("source")));
        vm.expectRevert(StrategyRegistry.NotTheBuilder.selector);
        strategies.register(
            id,
            author,
            keccak256("source"),
            keccak256("bundle"),
            keccak256("manifest"),
            chains,
            none,
            none,
            builderSig,
            clearance
        );

        bytes32 digestHere = strategies.registrationDigest(
            id, author, keccak256("source"), keccak256("bundle"), keccak256("manifest"), chains, none, none
        );
        vm.chainId(block.chainid + 1);
        assertTrue(
            strategies.registrationDigest(
                id,
                author,
                keccak256("source"),
                keccak256("bundle"),
                keccak256("manifest"),
                chains,
                none,
                none
            ) != digestHere
        );
    }

    // Prices --------------------------------------------------------------------

    /// The widest inputs the types allow integrate without overflowing: the
    /// 512-bit `mulDiv` carries the products, and the sum fits.
    function test_the_cost_of_the_largest_allowances_at_the_largest_prices_does_not_overflow() public {
        UnitPrices memory top = UnitPrices({
            vcpuHour: type(uint128).max,
            gibHour: type(uint128).max,
            gbEgress: type(uint128).max,
            inferenceSecond: type(uint128).max
        });
        vm.prank(admin);
        prices.schedule(top);
        skip(PRICE_NOTICE);
        Allowances memory a = Allowances({
            fuelPerSec: type(uint64).max,
            memoryMib: type(uint32).max,
            requestsPerMin: type(uint32).max,
            bytesPerDay: type(uint64).max,
            streams: type(uint32).max,
            inferenceMsPerMin: type(uint32).max,
            gasPerDay: type(uint64).max
        });
        uint256 whole = prices.costOf(a, 0, type(uint64).max);
        assertGt(whole, 0);
        assertGt(prices.dailyCost(a), 0);
    }

    /// Splitting an interval anywhere never charges more than the whole: each
    /// piece rounds down on its own, so the sum of pieces is at most the whole.
    function test_pieces_never_cost_more_than_the_whole() public view {
        Allowances memory a;
        a.fuelPerSec = 1; // a millionth of a core, so rounding bites
        a.gasPerDay = 7;
        uint64 t = uint64(vm.getBlockTimestamp());
        uint256 whole = prices.costOf(a, t, t + 1 days);
        uint256 pieces;
        for (uint64 i = 0; i < 24; i++) {
            pieces += prices.costOf(a, t + i * 1 hours, t + (i + 1) * 1 hours);
        }
        assertLe(pieces, whole);
    }

    // VaultFactory --------------------------------------------------------------

    /// A config change reaches only vaults activated after it; a running
    /// vault's terms are its own.
    function test_a_config_change_never_touches_a_running_vault() public {
        (Vault vault,) = openVault();
        VaultConfig memory before = factory.config();
        // A second read: a memory struct assigned from `before` would alias it.
        VaultConfig memory next = factory.config();
        next.holdingBps = 500;
        next.authorBps = 2000;
        next.transactionBps = 50;
        next.timers.freezeAfter = 1 days;
        vm.prank(admin);
        factory.setConfig(next);

        assertEq(vault.fees().holdingBps, before.holdingBps);
        assertEq(vault.fees().authorBps, before.authorBps);
        assertEq(vault.fees().transactionBps, before.transactionBps);
        assertEq(vault.timers().freezeAfter, before.timers.freezeAfter);
    }

    /// Activation is for the strategy's author alone and for a strategy with
    /// a real (non-refused) review; the review's bundle is the record's.
    function test_a_refused_strategy_never_activates_even_if_registered() public {
        // A clearance exists (one reviewer instance cleared the source), the
        // build registers, and a second reviewer instance refuses before the
        // review is posted. The record stands, but no vault can follow it.
        uint256 id = submitAs(author);
        vm.prank(reviewer);
        reviews.submitRefusal(id, reviewFor(bytes32(0), 3));
        register(id, author, keccak256(abi.encode("bundle", id)));
        assertTrue(strategies.isRegistered(id));
        assertFalse(reviews.hasReview(id));

        bytes32 terms = factory.terms();
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        vm.expectRevert();
        factory.activate(id, allowancesCosting(2e6), 500, MINIMUM_TANK, 0, "", terms);
        vm.stopPrank();
    }
}
