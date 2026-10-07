// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {ImageKind, KeyInfo} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";
import {MerkleProof} from "openzeppelin/utils/cryptography/MerkleProof.sol";

/// Each leaf of an instance's tree names the key it holds for that vault, so
/// one proof blesses every key the instance holds, each for its own vault
/// only, and the instance's own key (the journal's signer) for nothing.
///
/// Each test states what holds.
contract KeysInTheLeavesTest is Fixture {
    address internal instance = makeAddr("instance key");
    address internal keyA = makeAddr("key for vault A");
    address internal keyB = makeAddr("key for vault B");
    address internal keyC = makeAddr("key for vault C");
    address internal vaultA = makeAddr("vault A");
    address internal vaultB = makeAddr("vault B");
    /// A vault on another chain, served by its station here.
    address internal vaultC = makeAddr("vault C");
    uint256 internal constant OTHER_CHAIN = 8453;
    bytes32 internal bundle = keccak256("bundle");

    bytes32[3] internal leaves;
    bytes internal journal;
    bytes internal seal;

    function setUp() public override {
        super.setUp();
        leaves[0] = leafFor(vaultA, bundle, keyA);
        leaves[1] = leafFor(vaultB, bundle, keyB);
        leaves[2] = keyLeaf(OTHER_CHAIN, vaultC, bundle, keyC);
        journal = journalFor(EXECUTOR_IMAGE, instance, rootOf(leaves));
        seal = sealOf(journal);
    }

    function pair(bytes32 a, bytes32 b) internal pure returns (bytes32) {
        return a < b ? keccak256(abi.encodePacked(a, b)) : keccak256(abi.encodePacked(b, a));
    }

    /// The tree over three leaves, as OpenZeppelin's `MerkleProof` reads it:
    /// the first two paired, the third carried up and paired with them.
    function rootOf(bytes32[3] memory l) internal pure returns (bytes32) {
        return pair(pair(l[0], l[1]), l[2]);
    }

    function proofOf(bytes32[3] memory l, uint256 index) internal pure returns (bytes32[] memory proof) {
        if (index == 2) {
            proof = new bytes32[](1);
            proof[0] = pair(l[0], l[1]);
        } else {
            proof = new bytes32[](2);
            proof[0] = l[1 - index];
            proof[1] = l[2];
        }
    }

    function blessA() internal {
        attestation.bless(seal, journal, block.chainid, vaultA, bundle, keyA, proofOf(leaves, 0));
    }

    function test_one_proof_blesses_every_key_it_names_each_for_its_own_vault() public {
        blessA();
        attestation.bless(seal, journal, block.chainid, vaultB, bundle, keyB, proofOf(leaves, 1));
        attestation.bless(seal, journal, OTHER_CHAIN, vaultC, bundle, keyC, proofOf(leaves, 2));

        assertTrue(attestation.isBlessedFor(keyA, block.chainid, vaultA, bundle));
        assertTrue(attestation.isBlessedFor(keyB, block.chainid, vaultB, bundle));
        assertTrue(attestation.isBlessedFor(keyC, OTHER_CHAIN, vaultC, bundle));
        // Each for its own vault only.
        assertFalse(attestation.isBlessedFor(keyA, block.chainid, vaultB, bundle));
        assertFalse(attestation.isBlessedFor(keyB, block.chainid, vaultA, bundle));
        assertFalse(attestation.isBlessedFor(keyC, block.chainid, vaultA, bundle));
        assertEq(attestation.keyInfo(keyA).vault, vaultA);
        assertEq(attestation.keyInfo(keyB).vault, vaultB);
        assertEq(attestation.keyInfo(keyC).vault, vaultC);
        // The instance's own key names the instance and is blessed for nothing.
        assertTrue(attestation.keyInfo(instance).kind == ImageKind.None);
        assertFalse(attestation.isBlessedFor(instance, block.chainid, vaultA, bundle));
        assertFalse(attestation.isBlessedAs(instance, ImageKind.Executor));
    }

    function test_a_leaf_naming_one_key_never_blesses_another() public {
        // Key B with key A's leaf and proof.
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.bless(seal, journal, block.chainid, vaultA, bundle, keyB, proofOf(leaves, 0));
        // Key A at key B's vault, with key B's proof.
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.bless(seal, journal, block.chainid, vaultB, bundle, keyA, proofOf(leaves, 1));
        // The instance's own key, with any leaf.
        for (uint256 i = 0; i < 2; i++) {
            vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
            attestation.bless(
                seal, journal, block.chainid, i == 0 ? vaultA : vaultB, bundle, instance, proofOf(leaves, i)
            );
        }
        // A key nobody named.
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.bless(seal, journal, block.chainid, vaultA, bundle, alice, proofOf(leaves, 0));
        // And the zero key, whatever the leaf.
        vm.expectRevert(AttestationRegistry.NoSigner.selector);
        attestation.bless(seal, journal, block.chainid, vaultA, bundle, address(0), proofOf(leaves, 0));

        assertFalse(attestation.isBlessedFor(keyB, block.chainid, vaultA, bundle));
        assertFalse(attestation.isBlessedFor(keyA, block.chainid, vaultB, bundle));
        assertTrue(attestation.keyInfo(keyA).kind == ImageKind.None);
        assertTrue(attestation.keyInfo(keyB).kind == ImageKind.None);
    }

    function test_a_builder_s_and_a_reviewer_s_leaf_bless_the_key_it_names() public {
        address builderInstance = makeAddr("builder instance key");
        address builderKey = makeAddr("builder signing key");
        bytes memory builderJournal = journalFor(BUILDER_IMAGE, builderInstance, serviceLeaf(builderKey));
        attestation.bless(
            sealOf(builderJournal), builderJournal, 0, address(0), bytes32(0), builderKey, new bytes32[](0)
        );
        assertTrue(attestation.isBlessedAs(builderKey, ImageKind.Builder));
        assertFalse(attestation.isBlessedAs(builderInstance, ImageKind.Builder));

        address reviewerInstance = makeAddr("reviewer instance key");
        address reviewerKey = makeAddr("reviewer signing key");
        bytes memory reviewerJournal = journalFor(REVIEWER_IMAGE, reviewerInstance, serviceLeaf(reviewerKey));
        // The instance's own key isn't in the leaf, so it can't be blessed.
        bytes memory reviewerSeal = sealOf(reviewerJournal);
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.bless(
            reviewerSeal, reviewerJournal, 0, address(0), bytes32(0), reviewerInstance, new bytes32[](0)
        );
        attestation.bless(
            reviewerSeal, reviewerJournal, 0, address(0), bytes32(0), reviewerKey, new bytes32[](0)
        );
        assertTrue(attestation.isBlessedAs(reviewerKey, ImageKind.Reviewer));
        assertFalse(attestation.isBlessedAs(reviewerKey, ImageKind.Builder));
        assertFalse(attestation.isBlessedAs(reviewerInstance, ImageKind.Reviewer));
    }

    function test_a_key_blessed_from_one_image_or_project_is_refused_to_another_key_by_key() public {
        blessA();
        bytes32 otherImage = keccak256("another executor image");
        bytes32 otherProject = keccak256("another project");
        vm.startPrank(admin);
        attestation.proposeImage(otherImage, ImageKind.Executor);
        attestation.proposeProject(otherProject);
        vm.stopPrank();
        skip(TIMELOCK);

        // A second instance, of another image, names key A and a fresh key.
        address fresh = makeAddr("fresh key");
        bytes32[3] memory theirs;
        theirs[0] = leafFor(vaultA, bundle, keyA);
        theirs[1] = leafFor(vaultB, bundle, fresh);
        theirs[2] = leafFor(vaultB, bundle, keyB);
        bytes memory other = journalFor(otherImage, makeAddr("their instance"), rootOf(theirs));
        bytes memory otherSeal = sealOf(other);
        vm.expectRevert(AttestationRegistry.NotThisInstance.selector);
        attestation.bless(otherSeal, other, block.chainid, vaultA, bundle, keyA, proofOf(theirs, 0));
        // Its own fresh key is blessed; so is key B, which no image held yet.
        attestation.bless(otherSeal, other, block.chainid, vaultB, bundle, fresh, proofOf(theirs, 1));
        attestation.bless(otherSeal, other, block.chainid, vaultB, bundle, keyB, proofOf(theirs, 2));
        assertTrue(attestation.isBlessedFor(fresh, block.chainid, vaultB, bundle));
        assertEq(attestation.keyInfo(keyB).imageDigest, otherImage);
        // Key A stays with the image that first held it.
        assertEq(attestation.keyInfo(keyA).imageDigest, EXECUTOR_IMAGE);

        // The same image under another project can't claim key A either.
        bytes memory elsewhere = abi.encode(
            AttestationRegistry.Journal({
                imageDigest: EXECUTOR_IMAGE,
                projectId: otherProject,
                signer: instance,
                vaultsRoot: rootOf(leaves),
                issuedAt: uint64(block.timestamp)
            })
        );
        vm.expectRevert(AttestationRegistry.NotThisInstance.selector);
        attestation.bless(
            sealOf(elsewhere), elsewhere, block.chainid, vaultA, bundle, keyA, proofOf(leaves, 0)
        );
        assertEq(attestation.keyInfo(keyA).projectId, PROJECT);
    }

    function test_replaying_a_public_seal_neither_extends_nor_moves_a_blessing() public {
        blessA();
        uint64 until = attestation.blessedUntil(keyA, block.chainid, vaultA, bundle);
        assertEq(until, uint64(block.timestamp) + BLESSING_TTL);

        // A day on, a stranger replays it: nothing moves.
        skip(1 days);
        vm.prank(alice);
        blessA();
        assertEq(attestation.blessedUntil(keyA, block.chainid, vaultA, bundle), until);

        // Nor can the stranger point key A at another vault with the seal.
        vm.prank(alice);
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.bless(seal, journal, block.chainid, vaultB, bundle, keyA, proofOf(leaves, 1));
        // Blessing key B from the same seal leaves key A where it was.
        vm.prank(alice);
        attestation.bless(seal, journal, block.chainid, vaultB, bundle, keyB, proofOf(leaves, 1));
        KeyInfo memory info = attestation.keyInfo(keyA);
        assertEq(info.vault, vaultA);
        assertEq(info.blessedUntil, until);
        assertFalse(attestation.isBlessedFor(keyA, block.chainid, vaultB, bundle));

        // Past its TTL the seal blesses nothing.
        skip(BLESSING_TTL);
        vm.prank(alice);
        vm.expectRevert(AttestationRegistry.Stale.selector);
        blessA();
        assertFalse(attestation.isBlessedFor(keyA, block.chainid, vaultA, bundle));
    }

    /// An address of one byte repeated.
    function repeated(uint8 b) internal pure returns (address) {
        return address(uint160((type(uint160).max / 0xff) * b));
    }

    /// The vector the off-chain tree (`chain::attest`) is checked against:
    /// the same leaves, root and proof must come out of both.
    function test_the_leaf_and_the_tree_match_the_off_chain_vector() public {
        vm.chainId(31_337);
        bytes32 bundleHash = bytes32((type(uint256).max / 0xff) * 0x5b);
        bytes32[3] memory vector;
        vector[0] = keyLeaf(31_337, repeated(0x0a), bundleHash, repeated(0xe1));
        vector[1] = keyLeaf(31_337, repeated(0x0b), bundleHash, repeated(0xe2));
        vector[2] = keyLeaf(0, address(0), bytes32(0), repeated(0xe3));
        assertEq(vector[0], LEAF_0, "the leaf");
        assertEq(rootOf(vector), ROOT, "the root");
        for (uint256 i = 0; i < 3; i++) {
            assertTrue(MerkleProof.verify(proofOf(vector, i), ROOT, vector[i]));
        }
    }

    bytes32 internal constant LEAF_0 = 0x13bff5568b694d0a5bcff54736167b4950f3e40d1e91348b917fe483c6af5fb3;
    bytes32 internal constant ROOT = 0x84ab51bbb5eef0d7b04749b37284b518840833bdee69bad44be90f861b3c40cd;
}
