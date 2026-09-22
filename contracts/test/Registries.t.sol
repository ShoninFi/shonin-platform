// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {ImageKind, KeyInfo} from "../src/interfaces/IAttestationRegistry.sol";
import {Review} from "../src/interfaces/IReviewRegistry.sol";
import {VersionStatus} from "../src/interfaces/IStrategyRegistry.sol";
import {Fixture, MessageHashUtilsLite} from "./Fixture.sol";

/// What the registries are for: an image nobody had time to object to cannot
/// hold a key, a bundle nobody built cannot be registered, and a score nobody
/// measured cannot be posted.
contract RegistriesTest is Fixture {
    bytes32 internal constant STRATEGY = "funding-basis";

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
        vm.expectRevert();
        attestation.proposeImage(keccak256("mine"), ImageKind.Executor);
        vm.expectRevert();
        attestation.revokeImage(BUILDER_IMAGE);
    }

    function test_a_proof_nobody_signed_is_refused() public {
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, bytes32(0));
        vm.expectRevert(AttestationRegistry.ProofRejected.selector);
        attestation.bless(hex"", journal, address(0), bytes32(0), new bytes32[](0));
    }

    function test_an_image_off_the_allowlist_cannot_bless() public {
        bytes memory journal = journalFor(keccak256("unknown image"), executor, bytes32(0));
        vm.expectRevert(AttestationRegistry.ImageNotAllowed.selector);
        attestation.bless(sealOf(journal), journal, address(0), bytes32(0), new bytes32[](0));
    }

    function test_an_executor_key_is_blessed_for_one_vault_and_one_bundle() public {
        address vault = makeAddr("vault");
        bytes32 bundle = keccak256("bundle");
        blessExecutorKey(executor, vault, bundle);

        assertTrue(attestation.isBlessedFor(executor, vault, bundle));
        assertFalse(attestation.isBlessedFor(executor, makeAddr("another vault"), bundle));
        assertFalse(attestation.isBlessedFor(executor, vault, keccak256("another bundle")));

        KeyInfo memory info = attestation.keyInfo(executor);
        assertEq(info.imageDigest, EXECUTOR_IMAGE);
        assertEq(info.trustRoot, verifier.trustRoot());
        assertEq(info.blessedUntil, uint64(block.timestamp) + BLESSING_TTL);
    }

    function test_an_executor_cannot_claim_a_vault_it_did_not_attest_to() public {
        address mine = makeAddr("my vault");
        address yours = makeAddr("your vault");
        bytes32 bundle = keccak256("bundle");
        // The instance attested to holding a key for `mine`, then asks to be
        // blessed for `yours`.
        bytes32 leaf = keccak256(bytes.concat(keccak256(abi.encode(mine, bundle))));
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, leaf);
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.bless(sealOf(journal), journal, yours, bundle, new bytes32[](0));
    }

    function test_a_blessing_expires() public {
        address vault = makeAddr("vault");
        bytes32 bundle = keccak256("bundle");
        blessExecutorKey(executor, vault, bundle);
        skip(BLESSING_TTL + 1);
        assertFalse(attestation.isBlessedFor(executor, vault, bundle));
    }

    // The strategy registry -------------------------------------------------

    function signatureFor(uint256 key, bytes32 bundle) internal view returns (bytes memory) {
        bytes32 digest = strategies.registrationDigest(
            STRATEGY, 1, author, keccak256("source"), bundle, keccak256("manifest")
        );
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, MessageHashUtilsLite.toEthSignedMessageHash(digest));
        return abi.encodePacked(r, s, v);
    }

    function registerWith(uint256 key, bytes32 bundle) internal {
        registerSigned(bundle, signatureFor(key, bundle));
    }

    function registerSigned(bytes32 bundle, bytes memory signature) internal {
        strategies.register(
            STRATEGY, 1, author, keccak256("source"), bundle, keccak256("manifest"), bytes32(0), 0, signature
        );
    }

    function test_only_a_blessed_builder_registers_a_version() public {
        registerWith(BUILDER_KEY, keccak256("bundle"));
        assertEq(strategies.bundleHashOf(STRATEGY, 1), keccak256("bundle"));
        assertTrue(strategies.versionOf(STRATEGY, 1).status == VersionStatus.Registered);
    }

    function test_a_signature_from_anyone_else_is_refused() public {
        bytes memory signature = signatureFor(EXECUTOR_KEY, keccak256("bundle"));
        vm.expectRevert();
        registerSigned(keccak256("bundle"), signature);
    }

    function test_a_builder_cannot_register_a_different_bundle_than_it_signed() public {
        bytes32 digest = strategies.registrationDigest(
            STRATEGY, 1, author, keccak256("source"), keccak256("bundle"), keccak256("manifest")
        );
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(BUILDER_KEY, MessageHashUtilsLite.toEthSignedMessageHash(digest));
        bytes memory signature = abi.encodePacked(r, s, v);
        vm.expectRevert();
        strategies.register(
            STRATEGY,
            1,
            author,
            keccak256("source"),
            keccak256("a different bundle"),
            keccak256("manifest"),
            bytes32(0),
            0,
            signature
        );
    }

    function test_only_the_author_retires_a_version() public {
        registerWith(BUILDER_KEY, keccak256("bundle"));
        vm.expectRevert();
        strategies.retire(STRATEGY, 1);
        vm.prank(author);
        strategies.retire(STRATEGY, 1);
        assertTrue(strategies.versionOf(STRATEGY, 1).status == VersionStatus.Retired);
    }

    // Reviews ---------------------------------------------------------------

    function reviewOf(uint8 score, bytes32 bundle) internal view returns (Review memory) {
        return Review({
            bundleHash: bundle,
            sourceHash: keccak256("source"),
            reviewerImage: REVIEWER_IMAGE,
            modelIdHash: keccak256("model"),
            promptHash: keccak256("prompt"),
            reportHash: keccak256("report"),
            score: score,
            findingCount: 2,
            reviewedAt: uint64(block.timestamp),
            boughtBy: address(0)
        });
    }

    function test_a_review_costs_a_dollar_and_anyone_can_buy_one() public {
        give(alice, REVIEW_FEE);
        vm.startPrank(alice);
        usd.approve(address(reviews), REVIEW_FEE);
        reviews.requestReview(STRATEGY, 1);
        vm.stopPrank();
        assertEq(usd.balanceOf(platform), REVIEW_FEE);
    }

    function test_only_a_blessed_reviewer_posts_a_score() public {
        registerWith(BUILDER_KEY, keccak256("bundle"));
        vm.prank(alice);
        vm.expectRevert();
        reviews.submitReview(STRATEGY, 1, reviewOf(80, keccak256("bundle")));

        vm.prank(reviewer);
        reviews.submitReview(STRATEGY, 1, reviewOf(80, keccak256("bundle")));
        assertEq(reviews.latestReview(STRATEGY, 1).score, 80);
    }

    function test_a_score_cannot_follow_code_nobody_scored() public {
        registerWith(BUILDER_KEY, keccak256("bundle"));
        vm.prank(reviewer);
        vm.expectRevert();
        reviews.submitReview(STRATEGY, 1, reviewOf(99, keccak256("some other bundle")));
    }

    function test_reviews_accumulate_rather_than_replace() public {
        registerWith(BUILDER_KEY, keccak256("bundle"));
        vm.startPrank(reviewer);
        reviews.submitReview(STRATEGY, 1, reviewOf(42, keccak256("bundle")));
        reviews.submitReview(STRATEGY, 1, reviewOf(91, keccak256("bundle")));
        vm.stopPrank();
        assertEq(reviews.reviewCount(STRATEGY, 1), 2);
        assertEq(reviews.reviewAt(STRATEGY, 1, 0).score, 42);
        assertEq(reviews.latestReview(STRATEGY, 1).score, 91);
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
