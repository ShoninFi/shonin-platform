// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {ReviewRegistry} from "../src/ReviewRegistry.sol";
import {StrategyRegistry} from "../src/StrategyRegistry.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Review} from "../src/interfaces/IReviewRegistry.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// Inputs that mean "none" elsewhere are refused as values.
contract RegistryInputsTest is Fixture {
    /// `proposeImage` accepts `ImageKind.None`. A key blessed from such an
    /// image has `kind == None`, which is also what "never blessed" looks
    /// like, so the `NotThisInstance` guard (a key belongs to one image and
    /// one project, forever) does not bind for it: a later journal from any
    /// other image can take the same signer over. Both journals need a seal,
    /// so this is an admin-input gap rather than a stranger's attack, but the
    /// enum's zero is a sentinel and should be refused.
    function test_an_image_cannot_be_proposed_as_no_kind() public {
        bytes32 image = keccak256("mistyped kind");
        vm.prank(admin);
        vm.expectRevert();
        attestation.proposeImage(image, ImageKind.None);
    }

    function test_an_image_must_name_its_kind() public {
        vm.prank(admin);
        vm.expectRevert(AttestationRegistry.NoKind.selector);
        attestation.proposeImage(keccak256("mistyped kind"), ImageKind.None);
    }

    /// `register` accepts a zero `bundleHash`, but `bundleHashOf` documents
    /// zero as "not registered", `submitRefusal` uses a zero bundle to mean a
    /// refusal, and the vault is created around whatever bundle the record
    /// holds. A builder would have to sign it, so this is a consistency gap,
    /// not an exploit; the registry should refuse zero hashes.
    function test_a_strategy_cannot_register_with_no_bundle() public {
        uint256 id = submitAs(author);
        ChainNeed[] memory chains = oneChain();
        bytes32[] memory none = new bytes32[](0);
        bytes32 digest = strategies.registrationDigest(
            id, author, keccak256("source"), bytes32(0), keccak256("manifest"), chains, none, none
        );
        bytes memory builderSig = signed(BUILDER_KEY, digest);
        bytes memory clearance = signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")));
        vm.expectRevert();
        strategies.register(
            id,
            author,
            keccak256("source"),
            bytes32(0),
            keccak256("manifest"),
            chains,
            none,
            none,
            builderSig,
            clearance
        );
    }

    /// A refusal is recorded against the source it read, so it must name one.
    function test_a_refusal_names_the_source_it_read() public {
        uint256 id = submitAs(author);
        Review memory refusal = reviewFor(bytes32(0), 3);
        refusal.sourceHash = bytes32(0);
        vm.prank(reviewer);
        vm.expectRevert(ReviewRegistry.NoSource.selector);
        reviews.submitRefusal(id, refusal);
    }
}
