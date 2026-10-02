// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {DevAttestationVerifier} from "../src/DevAttestationVerifier.sol";
import {ReviewRegistry} from "../src/ReviewRegistry.sol";

import {Station} from "../src/Station.sol";
import {StrategyRegistry} from "../src/StrategyRegistry.sol";
import {Vault} from "../src/Vault.sol";
import {ImageKind, KeyInfo} from "../src/interfaces/IAttestationRegistry.sol";
import {Rating, Review} from "../src/interfaces/IReviewRegistry.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// What the registries are for: an image nobody had time to object to cannot
/// hold a key, a bundle nobody built from source nobody read cannot be
/// registered, and a review nobody wrote cannot be posted.
contract RegistriesTest is Fixture {
    // The allowlist ---------------------------------------------------------

    function test_an_image_waits_out_its_timelock() public {
        bytes32 image = keccak256("a new executor");
        vm.prank(admin);
        attestation.proposeImage(image, ImageKind.Executor);

        assertFalse(attestation.isImageAllowed(image));
        skip(TIMELOCK - 1);
        assertFalse(attestation.isImageAllowed(image));
        skip(1);
        assertTrue(attestation.isImageAllowed(image));
    }

    function test_revoking_an_image_takes_effect_at_once() public {
        assertTrue(attestation.isImageAllowed(BUILDER_IMAGE));
        vm.prank(admin);
        attestation.revokeImage(BUILDER_IMAGE);
        assertFalse(attestation.isImageAllowed(BUILDER_IMAGE));
        // And the keys it holds stop counting in the same instant.
        assertFalse(attestation.isBlessedAs(builder, ImageKind.Builder));
    }

    function test_only_the_admin_changes_the_allowlist() public {
        vm.expectRevert(AttestationRegistry.NotAdmin.selector);
        attestation.proposeImage(keccak256("mine"), ImageKind.Executor);
        vm.expectRevert(AttestationRegistry.NotAdmin.selector);
        attestation.revokeImage(BUILDER_IMAGE);
        vm.expectRevert(AttestationRegistry.NotAdmin.selector);
        attestation.proposeDriver("local", address(1));
        vm.expectRevert(AttestationRegistry.NotAdmin.selector);
        attestation.proposeVerifier(address(1));
        vm.expectRevert(AttestationRegistry.NotAdmin.selector);
        attestation.setTimelock(0);
    }

    // The timelock ----------------------------------------------------------

    function test_raising_the_timelock_applies_at_once() public {
        vm.prank(admin);
        attestation.setTimelock(TIMELOCK * 2);
        assertEq(attestation.timelock(), TIMELOCK * 2);
        (uint64 pending,) = attestation.pendingTimelock();
        assertEq(pending, 0);
    }

    function test_lowering_the_timelock_waits_out_the_current_one() public {
        vm.prank(admin);
        attestation.setTimelock(1 hours);
        assertEq(attestation.timelock(), TIMELOCK);
        (uint64 pending, uint64 at) = attestation.pendingTimelock();
        assertEq(pending, 1 hours);
        assertEq(at, block.timestamp + TIMELOCK);

        // Anything proposed meanwhile still waits the old, longer timelock.
        bytes32 image = keccak256("slipped in");
        vm.prank(admin);
        attestation.proposeImage(image, ImageKind.Executor);
        skip(TIMELOCK - 1);
        assertEq(attestation.timelock(), TIMELOCK);
        assertFalse(attestation.isImageAllowed(image));
        skip(1);
        assertEq(attestation.timelock(), 1 hours);
        assertTrue(attestation.isImageAllowed(image));
    }

    function test_a_raise_cancels_a_pending_lowering() public {
        vm.startPrank(admin);
        attestation.setTimelock(1 hours);
        attestation.setTimelock(TIMELOCK);
        vm.stopPrank();
        skip(TIMELOCK);
        assertEq(attestation.timelock(), TIMELOCK);
    }

    // Attestation checkers --------------------------------------------------

    function test_a_new_checker_waits_out_the_timelock_and_the_old_one_keeps_working() public {
        uint256 newAttestorKey = 0xBEEF;
        DevAttestationVerifier second = new DevAttestationVerifier(vm.addr(newAttestorKey));
        vm.prank(admin);
        attestation.proposeVerifier(address(second));
        assertFalse(attestation.isVerifierAllowed(address(second)));

        bytes memory journal = journalFor(BUILDER_IMAGE, makeAddr("next builder"), serviceLeaf());
        bytes memory proof = signedJournal(newAttestorKey, journal);
        vm.expectRevert(AttestationRegistry.ProofRejected.selector);
        attestation.bless(proof, journal, 0, address(0), bytes32(0), new bytes32[](0));

        skip(TIMELOCK);
        attestation.bless(proof, journal, 0, address(0), bytes32(0), new bytes32[](0));
        assertTrue(attestation.isBlessedAs(makeAddr("next builder"), ImageKind.Builder));
        // The first checker still blesses.
        blessServiceKey(BUILDER_IMAGE, makeAddr("another builder"));
    }

    function test_retiring_a_checker_is_immediate() public {
        vm.prank(admin);
        attestation.retireVerifier(address(verifier));
        bytes memory journal = journalFor(BUILDER_IMAGE, makeAddr("next builder"), serviceLeaf());
        vm.expectRevert(AttestationRegistry.ProofRejected.selector);
        attestation.bless(sealOf(journal), journal, 0, address(0), bytes32(0), new bytes32[](0));
    }

    function signedJournal(uint256 key, bytes memory journal) internal pure returns (bytes memory) {
        return signed(key, keccak256(journal));
    }

    function test_a_checker_that_answers_nonsense_is_skipped() public {
        // An account with no code answers a call with nothing.
        vm.prank(admin);
        attestation.proposeVerifier(makeAddr("not a checker"));
        skip(TIMELOCK);
        blessServiceKey(BUILDER_IMAGE, makeAddr("still blessable"));
        assertTrue(attestation.isBlessedAs(makeAddr("still blessable"), ImageKind.Builder));
    }

    function test_a_key_from_one_instance_cannot_be_claimed_by_another_image() public {
        bytes memory journal = journalFor(REVIEWER_IMAGE, builder, serviceLeaf());
        bytes memory proof = sealOf(journal);
        vm.expectRevert(AttestationRegistry.NotThisInstance.selector);
        attestation.bless(proof, journal, 0, address(0), bytes32(0), new bytes32[](0));
    }

    function test_an_unwinder_key_is_never_approved_to_trade() public {
        bytes32 unwinderImage = keccak256("unwinder image");
        vm.prank(admin);
        attestation.proposeImage(unwinderImage, ImageKind.Unwinder);
        skip(TIMELOCK);
        blessServiceKey(BUILDER_IMAGE, builder);
        blessServiceKey(REVIEWER_IMAGE, reviewer);
        (Vault vault, bytes32 bundle) = openVault();
        Station station = stationOf(vault);
        address unwinder = makeAddr("unwinder");
        bytes memory journal = journalFor(unwinderImage, unwinder, leafFor(address(vault), bundle));
        attestation.bless(sealOf(journal), journal, block.chainid, address(vault), bundle, new bytes32[](0));
        assertTrue(attestation.isBlessedFor(unwinder, block.chainid, address(vault), bundle));
        vm.expectRevert(Station.NotBlessed.selector);
        station.approveKey("local", unwinder);
    }

    // Drivers and bridges ---------------------------------------------------

    function test_a_driver_waits_out_the_timelock() public {
        address v1 = makeAddr("driver v1");
        vm.prank(admin);
        attestation.proposeDriver("test-driver", v1);
        assertEq(attestation.tradingDriver("test-driver"), address(0));
        skip(TIMELOCK);
        assertEq(attestation.tradingDriver("test-driver"), v1);
    }

    function test_a_new_driver_version_replaces_the_old_only_when_ready() public {
        address v1 = makeAddr("driver v1");
        address v2 = makeAddr("driver v2");
        vm.prank(admin);
        attestation.proposeDriver("test-driver", v1);
        skip(TIMELOCK);
        vm.prank(admin);
        attestation.proposeDriver("test-driver", v2);
        assertEq(attestation.tradingDriver("test-driver"), v1);
        skip(TIMELOCK);
        assertEq(attestation.tradingDriver("test-driver"), v2);
    }

    function test_revoking_a_driver_stops_new_stations_pinning_it() public {
        address v1 = makeAddr("driver v1");
        vm.prank(admin);
        attestation.proposeDriver("test-driver", v1);
        skip(TIMELOCK);
        vm.prank(admin);
        attestation.revokeDriver("test-driver", v1);
        assertEq(attestation.tradingDriver("test-driver"), address(0));
        assertTrue(attestation.isDriverRevoked("test-driver", v1));
    }

    function test_withdrawing_a_proposal_leaves_the_driver_in_force() public {
        address v1 = makeAddr("driver v1");
        address bad = makeAddr("bad driver");
        vm.startPrank(admin);
        attestation.proposeDriver("test-driver", v1);
        skip(TIMELOCK);
        attestation.proposeDriver("test-driver", bad);
        attestation.withdrawDriverProposal("test-driver");
        vm.stopPrank();
        skip(TIMELOCK);
        // The waiting proposal never counts, and the one in force stands.
        assertEq(attestation.tradingDriver("test-driver"), v1);
        assertFalse(attestation.isDriverRevoked("test-driver", v1));
    }

    function test_an_older_driver_stations_still_pin_can_be_revoked() public {
        address v1 = makeAddr("driver v1");
        address v2 = makeAddr("driver v2");
        vm.startPrank(admin);
        attestation.proposeDriver("test-driver", v1);
        skip(TIMELOCK);
        attestation.proposeDriver("test-driver", v2);
        skip(TIMELOCK);
        attestation.revokeDriver("test-driver", v1);
        vm.stopPrank();
        assertTrue(attestation.isDriverRevoked("test-driver", v1));
        assertEq(attestation.tradingDriver("test-driver"), v2, "revoking an older one touched the newer");
    }

    function test_revoking_a_proposal_still_waiting_withdraws_it() public {
        address v1 = makeAddr("driver v1");
        vm.startPrank(admin);
        attestation.proposeDriver("test-driver", v1);
        attestation.revokeDriver("test-driver", v1);
        vm.stopPrank();
        skip(TIMELOCK);
        assertEq(attestation.tradingDriver("test-driver"), address(0), "the revoked proposal went live");
    }

    function test_a_fix_proposed_after_a_revocation_trades_again() public {
        address v1 = makeAddr("driver v1");
        address v2 = makeAddr("driver v2");
        vm.startPrank(admin);
        attestation.proposeDriver("test-driver", v1);
        skip(TIMELOCK);
        attestation.revokeDriver("test-driver", v1);
        attestation.proposeDriver("test-driver", v2);
        vm.stopPrank();
        skip(TIMELOCK);
        assertEq(attestation.tradingDriver("test-driver"), v2);
    }

    function test_the_first_bridge_proposal_can_be_withdrawn() public {
        vm.startPrank(admin);
        attestation.proposeBridge("test-bridge", makeAddr("wrong bridge"));
        attestation.withdrawBridgeProposal("test-bridge");
        vm.stopPrank();
        skip(TIMELOCK);
        assertEq(attestation.bridgeOf("test-bridge"), address(0));
    }

    function test_bridges_are_approved_the_same_way() public {
        address bridge = makeAddr("bridge");
        vm.prank(admin);
        attestation.proposeBridge("test-bridge", bridge);
        assertEq(attestation.bridgeOf("test-bridge"), address(0));
        skip(TIMELOCK);
        assertEq(attestation.bridgeOf("test-bridge"), bridge);
        vm.prank(admin);
        attestation.revokeBridge("test-bridge", bridge);
        assertEq(attestation.bridgeOf("test-bridge"), address(0));
        assertTrue(attestation.isBridgeRevoked("test-bridge", bridge));
    }

    // Blessing --------------------------------------------------------------

    function test_a_proof_nobody_signed_is_refused() public {
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, bytes32(0));
        vm.expectRevert(AttestationRegistry.ProofRejected.selector);
        attestation.bless(hex"", journal, 0, address(0), bytes32(0), new bytes32[](0));
    }

    function test_an_image_off_the_allowlist_cannot_bless() public {
        bytes memory journal = journalFor(keccak256("unknown image"), executor, bytes32(0));
        vm.expectRevert(AttestationRegistry.ImageNotAllowed.selector);
        attestation.bless(sealOf(journal), journal, 0, address(0), bytes32(0), new bytes32[](0));
    }

    function test_an_executor_key_is_blessed_for_one_vault_and_one_bundle() public {
        address vault = makeAddr("vault");
        bytes32 bundle = keccak256("bundle");
        blessExecutorKey(executor, vault, bundle);

        assertTrue(attestation.isBlessedFor(executor, block.chainid, vault, bundle));
        assertFalse(attestation.isBlessedFor(executor, block.chainid, makeAddr("another vault"), bundle));
        assertFalse(attestation.isBlessedFor(executor, block.chainid, vault, keccak256("another bundle")));
        // The same address on another vault chain is another vault.
        assertFalse(attestation.isBlessedFor(executor, block.chainid + 1, vault, bundle));

        KeyInfo memory info = attestation.keyInfo(executor);
        assertEq(info.imageDigest, EXECUTOR_IMAGE);
        assertEq(info.vaultChain, block.chainid);
        assertEq(info.trustRoot, verifier.trustRoot());
        assertEq(info.blessedUntil, uint64(block.timestamp) + BLESSING_TTL);
    }

    function test_an_executor_cannot_claim_a_vault_it_did_not_attest_to() public {
        address mine = makeAddr("my vault");
        address yours = makeAddr("your vault");
        bytes32 bundle = keccak256("bundle");
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, leafFor(mine, bundle));
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.bless(sealOf(journal), journal, block.chainid, yours, bundle, new bytes32[](0));
    }

    function test_a_blessing_proved_for_one_chain_cannot_be_replayed_on_another() public {
        address vault = makeAddr("vault");
        bytes32 bundle = keccak256("bundle");
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, leafFor(vault, bundle));
        bytes memory proof = sealOf(journal);
        vm.chainId(block.chainid + 7);
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.bless(proof, journal, 31_337, vault, bundle, new bytes32[](0));
    }

    function test_a_service_key_names_no_vault() public {
        bytes memory journal = journalFor(BUILDER_IMAGE, makeAddr("builder two"), serviceLeaf());
        bytes memory proof = sealOf(journal);
        vm.expectRevert(AttestationRegistry.WrongKindForVault.selector);
        attestation.bless(proof, journal, block.chainid, address(0), bytes32(0), new bytes32[](0));
    }

    function test_a_blessing_expires() public {
        address vault = makeAddr("vault");
        bytes32 bundle = keccak256("bundle");
        blessExecutorKey(executor, vault, bundle);
        skip(BLESSING_TTL + 1);
        assertFalse(attestation.isBlessedFor(executor, block.chainid, vault, bundle));
    }

    // The strategy registry -------------------------------------------------

    function registerWith(uint256 id, uint256 builderKey, uint256 reviewerKey) internal {
        registerExpecting(id, builderKey, reviewerKey, bytes4(0));
    }

    /// Sign first, then expect the revert, so the expectation lands on the
    /// registration and not on the views the signatures are made from.
    function registerExpecting(uint256 id, uint256 builderKey, uint256 reviewerKey, bytes4 error) internal {
        ChainNeed[] memory chains = oneChain();
        bytes32[] memory none = new bytes32[](0);
        bytes32 digest = strategies.registrationDigest(
            id, author, keccak256("source"), keccak256("bundle"), keccak256("manifest"), chains, none, none
        );
        bytes memory builderSig = signed(builderKey, digest);
        bytes memory clearance = signed(reviewerKey, strategies.clearanceDigest(id, keccak256("source")));
        if (error != bytes4(0)) vm.expectRevert(error);
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
    }

    function test_a_strategy_registers_with_a_builder_signature_and_a_reviewer_go_ahead() public {
        uint256 id = submitAs(author);
        registerWith(id, BUILDER_KEY, REVIEWER_KEY);
        assertTrue(strategies.isRegistered(id));
        assertEq(strategies.bundleHashOf(id), keccak256("bundle"));
        assertEq(strategies.authorOf(id), author);
        assertEq(strategies.strategyOf(id).chains[0].chain, bytes32("base"));
        assertEq(strategies.strategyOf(id).chains[0].drivers[0], bytes32("local"));
    }

    function test_a_signature_from_anyone_but_a_builder_is_refused() public {
        uint256 id = submitAs(author);
        registerExpecting(id, EXECUTOR_KEY, REVIEWER_KEY, StrategyRegistry.NotTheBuilder.selector);
    }

    function test_nothing_registers_without_the_reviewer_go_ahead() public {
        uint256 id = submitAs(author);
        registerExpecting(id, BUILDER_KEY, BUILDER_KEY, StrategyRegistry.NotCleared.selector);
    }

    function test_a_go_ahead_is_for_one_strategy_and_one_source() public {
        uint256 id = submitAs(author);
        uint256 other = submitAs(author);
        ChainNeed[] memory chains = oneChain();
        bytes32[] memory none = new bytes32[](0);
        bytes32 digest = strategies.registrationDigest(
            id, author, keccak256("source"), keccak256("bundle"), keccak256("manifest"), chains, none, none
        );
        bytes memory clearanceForOther =
            signed(REVIEWER_KEY, strategies.clearanceDigest(other, keccak256("source")));
        bytes memory builderSig = signed(BUILDER_KEY, digest);
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
            clearanceForOther
        );
    }

    function test_a_builder_cannot_register_a_different_bundle_than_it_signed() public {
        uint256 id = submitAs(author);
        ChainNeed[] memory chains = oneChain();
        bytes32[] memory none = new bytes32[](0);
        bytes32 digest = strategies.registrationDigest(
            id, author, keccak256("source"), keccak256("bundle"), keccak256("manifest"), chains, none, none
        );
        bytes memory builderSig = signed(BUILDER_KEY, digest);
        bytes memory clearance = signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")));
        vm.expectRevert(StrategyRegistry.NotTheBuilder.selector);
        strategies.register(
            id,
            author,
            keccak256("source"),
            keccak256("a different bundle"),
            keccak256("manifest"),
            chains,
            none,
            none,
            builderSig,
            clearance
        );
    }

    function test_the_chains_are_part_of_what_the_builder_signed() public {
        uint256 id = submitAs(author);
        ChainNeed[] memory chains = oneChain();
        bytes32[] memory none = new bytes32[](0);
        bytes32 digest = strategies.registrationDigest(
            id, author, keccak256("source"), keccak256("bundle"), keccak256("manifest"), chains, none, none
        );
        chains[0].drivers[0] = "something-else";
        bytes memory builderSig = signed(BUILDER_KEY, digest);
        bytes memory clearance = signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")));
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
    }

    function test_an_id_is_registered_only_for_the_wallet_that_paid_for_it() public {
        uint256 id = submitAs(alice);
        registerExpecting(id, BUILDER_KEY, REVIEWER_KEY, StrategyRegistry.NotSubmittedByTheAuthor.selector);
        // And an id nobody paid for registers for nobody.
        registerExpecting(99, BUILDER_KEY, REVIEWER_KEY, StrategyRegistry.NotSubmittedByTheAuthor.selector);
    }

    function test_a_strategy_registers_once() public {
        uint256 id = submitAs(author);
        registerWith(id, BUILDER_KEY, REVIEWER_KEY);
        registerExpecting(id, BUILDER_KEY, REVIEWER_KEY, StrategyRegistry.AlreadyRegistered.selector);
    }

    function test_the_restricted_list_is_sorted_and_without_repeats() public {
        uint256 id = submitAs(author);
        ChainNeed[] memory chains = oneChain();
        bytes32[] memory none = new bytes32[](0);
        bytes32[] memory restricted = new bytes32[](2);
        restricted[0] = "US";
        restricted[1] = "US";
        bytes32 digest = strategies.registrationDigest(
            id,
            author,
            keccak256("source"),
            keccak256("bundle"),
            keccak256("manifest"),
            chains,
            none,
            restricted
        );
        bytes memory builderSig = signed(BUILDER_KEY, digest);
        bytes memory clearance = signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")));
        vm.expectRevert(StrategyRegistry.NotSorted.selector);
        strategies.register(
            id,
            author,
            keccak256("source"),
            keccak256("bundle"),
            keccak256("manifest"),
            chains,
            none,
            restricted,
            builderSig,
            clearance
        );
    }

    // Reviews ---------------------------------------------------------------

    function test_only_a_blessed_reviewer_posts_a_review() public {
        uint256 id = submitAs(author);
        registerWith(id, BUILDER_KEY, REVIEWER_KEY);
        vm.prank(alice);
        vm.expectRevert(ReviewRegistry.NotTheReviewer.selector);
        reviews.submitReview(id, reviewFor(keccak256("bundle"), uint8(Rating.LooksFine)));

        Review memory review = reviewFor(keccak256("bundle"), uint8(Rating.LooksFine));
        review.ratings[1] = uint8(Rating.SeriousConcerns);
        vm.prank(reviewer);
        reviews.submitReview(id, review);
        (Review memory stored, bool refused) = reviews.reviewOf(id);
        assertFalse(refused);
        assertTrue(reviews.hasReview(id));
        assertEq(stored.ratings[1], uint8(Rating.SeriousConcerns));
        assertEq(stored.ratings[0], uint8(Rating.LooksFine));
    }

    function test_a_review_cannot_follow_code_nobody_read() public {
        uint256 id = submitAs(author);
        registerWith(id, BUILDER_KEY, REVIEWER_KEY);
        vm.prank(reviewer);
        vm.expectRevert(ReviewRegistry.WrongBundle.selector);
        reviews.submitReview(id, reviewFor(keccak256("some other bundle"), 1));
    }

    function test_a_strategy_is_reviewed_once() public {
        uint256 id = submitAs(author);
        registerWith(id, BUILDER_KEY, REVIEWER_KEY);
        vm.startPrank(reviewer);
        reviews.submitReview(id, reviewFor(keccak256("bundle"), 1));
        vm.expectRevert(ReviewRegistry.AlreadyReviewed.selector);
        reviews.submitReview(id, reviewFor(keccak256("bundle"), 2));
        vm.stopPrank();
    }

    function test_every_area_carries_a_rating() public {
        uint256 id = submitAs(author);
        registerWith(id, BUILDER_KEY, REVIEWER_KEY);
        Review memory review = reviewFor(keccak256("bundle"), 1);
        review.ratings[5] = 0;
        vm.prank(reviewer);
        vm.expectRevert(ReviewRegistry.BadRating.selector);
        reviews.submitReview(id, review);
        review.ratings[5] = 5;
        vm.prank(reviewer);
        vm.expectRevert(ReviewRegistry.BadRating.selector);
        reviews.submitReview(id, review);
    }

    function test_a_refusal_is_recorded_against_the_source_and_nothing_registers_after() public {
        uint256 id = submitAs(author);
        Review memory refusal = reviewFor(bytes32(0), uint8(Rating.CouldntTell));
        refusal.ratings[0] = uint8(Rating.SeriousConcerns);
        vm.prank(reviewer);
        reviews.submitRefusal(id, refusal);
        (Review memory stored, bool refused) = reviews.reviewOf(id);
        assertTrue(refused);
        assertFalse(reviews.hasReview(id));
        assertEq(stored.sourceHash, keccak256("source"));
    }

    function test_a_refusal_is_only_for_a_submitted_strategy_not_yet_registered() public {
        Review memory refusal = reviewFor(bytes32(0), 1);
        vm.prank(reviewer);
        vm.expectRevert(ReviewRegistry.NotSubmitted.selector);
        reviews.submitRefusal(42, refusal);

        uint256 id = submitAs(author);
        registerWith(id, BUILDER_KEY, REVIEWER_KEY);
        vm.prank(reviewer);
        vm.expectRevert(ReviewRegistry.AlreadyRegistered.selector);
        reviews.submitRefusal(id, refusal);
    }

    // Authors ---------------------------------------------------------------

    function test_a_handle_belongs_to_one_author() public {
        string[] memory labels = new string[](1);
        string[] memory urls = new string[](1);
        labels[0] = "Notes";
        urls[0] = "https://example.invalid/notes";

        vm.prank(author);
        profiles.setProfile("carry", "I trade basis.", labels, urls);
        assertEq(profiles.authorOfHandle("carry"), author);
        assertEq(profiles.profileOf(author).linkUrls[0], "https://example.invalid/notes");

        vm.prank(alice);
        vm.expectRevert();
        profiles.setProfile("carry", "mine now", labels, urls);
    }
}
