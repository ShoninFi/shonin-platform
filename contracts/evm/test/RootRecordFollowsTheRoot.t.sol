// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// The root passes between enclaves by attested exchange, so more than one
/// allowed image can hold the same root at once: an executor image and the
/// image that replaces it, or two generations of holders. The record names
/// the image that last recorded it, and is live only while that image is
/// allowed. So the record has to follow the root: an instance of another
/// allowed executor or holder image, holding the same root (it can only
/// have it from a holder that checked its image against this registry),
/// attests it afresh, and the record moves to that image, as a blessing
/// moves to a later attestation. Otherwise revoking the first image, the
/// routine end of a rotation once its successor runs, darkens a root its
/// successor holds and serves, and the only way out is a clear, which
/// retires that root for good and has every sealed secret sealed again.
///
/// What doesn't move: another root while one is recorded, an earlier
/// attestation, and a cleared root, which no image can bring back.
contract RootRecordFollowsTheRootTest is Fixture {
    bytes32 internal constant NEXT_EXECUTOR_IMAGE = keccak256("the next executor image");
    bytes32 internal constant HOLDER_IMAGE = keccak256("holder image");

    address internal instance = makeAddr("instance key");
    address internal successor = makeAddr("the successor's instance key");
    address internal root = makeAddr("root signing key");

    function rootLeaf(address signer) internal view returns (bytes32) {
        return keyLeaf(0, address(0), attestation.ROOT_KEY_PURPOSE(), signer);
    }

    function setUp() public override {
        super.setUp();
        vm.startPrank(admin);
        attestation.proposeImage(NEXT_EXECUTOR_IMAGE, ImageKind.Executor);
        attestation.proposeImage(HOLDER_IMAGE, ImageKind.Holder);
        vm.stopPrank();
        skip(TIMELOCK);
        // The first executor image records the root.
        bytes memory j = journalFor(EXECUTOR_IMAGE, instance, rootLeaf(root));
        attestation.setRootKey(sealOf(j), j, root, new bytes32[](0));
        assertTrue(attestation.isRootKeyLive());
        assertEq(attestation.rootKey().imageDigest, EXECUTOR_IMAGE);
    }

    /// The same root, attested later by an instance of the next allowed
    /// executor image: the record moves to that image, and revoking the
    /// first image afterwards leaves the root live.
    function test_a_later_attestation_of_the_same_root_from_another_allowed_image_moves_the_record() public {
        skip(1 hours);
        bytes memory j = journalFor(NEXT_EXECUTOR_IMAGE, successor, rootLeaf(root));
        attestation.setRootKey(sealOf(j), j, root, new bytes32[](0));
        assertEq(attestation.rootKey().signer, root);
        assertEq(attestation.rootKey().imageDigest, NEXT_EXECUTOR_IMAGE);

        vm.prank(admin);
        attestation.revokeImage(EXECUTOR_IMAGE);
        assertTrue(attestation.isRootKeyLive());
    }

    /// The first image is revoked before its successor attests: the record
    /// is dark, and the successor's attestation of the same root, from an
    /// image still allowed, brings it back without a clear.
    function test_revoking_the_first_image_does_not_darken_a_root_its_successor_attests() public {
        vm.prank(admin);
        attestation.revokeImage(EXECUTOR_IMAGE);
        assertFalse(attestation.isRootKeyLive());

        skip(1);
        bytes memory j = journalFor(NEXT_EXECUTOR_IMAGE, successor, rootLeaf(root));
        attestation.setRootKey(sealOf(j), j, root, new bytes32[](0));
        assertTrue(attestation.isRootKeyLive());
        assertEq(attestation.rootKey().imageDigest, NEXT_EXECUTOR_IMAGE);
    }

    /// A dedicated holder that received the root from the executor can carry
    /// its record too, so the executor image can be retired once holders run.
    function test_a_holder_that_received_the_root_can_carry_its_record() public {
        skip(1 hours);
        bytes memory j = journalFor(HOLDER_IMAGE, successor, rootLeaf(root));
        attestation.setRootKey(sealOf(j), j, root, new bytes32[](0));
        assertEq(attestation.rootKey().imageDigest, HOLDER_IMAGE);

        vm.prank(admin);
        attestation.revokeImage(EXECUTOR_IMAGE);
        assertTrue(attestation.isRootKeyLive());
    }

    /// What must not change: another root is still refused while one is
    /// recorded, whatever image attests it, and an attestation earlier than
    /// the recorded one moves nothing.
    function test_another_root_or_an_earlier_attestation_still_moves_nothing() public {
        address another = makeAddr("another root");
        skip(1 hours);
        bytes memory j = journalFor(NEXT_EXECUTOR_IMAGE, successor, rootLeaf(another));
        vm.expectRevert(AttestationRegistry.RootKeyAlreadySet.selector);
        attestation.setRootKey(sealOf(j), j, another, new bytes32[](0));

        // Dated an hour before the record's attestation.
        j = abi.encode(
            AttestationRegistry.Journal({
                imageDigest: NEXT_EXECUTOR_IMAGE,
                projectId: PROJECT,
                signer: successor,
                vaultsRoot: rootLeaf(root),
                issuedAt: uint64(block.timestamp - 2 hours)
            })
        );
        vm.expectRevert(AttestationRegistry.RootKeyAlreadySet.selector);
        attestation.setRootKey(sealOf(j), j, root, new bytes32[](0));
        assertEq(attestation.rootKey().imageDigest, EXECUTOR_IMAGE);
    }

    /// A cleared root stays retired for good, whichever allowed image
    /// attests it afterwards.
    function test_a_cleared_root_is_not_brought_back_by_another_image() public {
        skip(1 hours);
        bytes memory j = journalFor(NEXT_EXECUTOR_IMAGE, successor, rootLeaf(root));
        attestation.setRootKey(sealOf(j), j, root, new bytes32[](0));
        vm.prank(admin);
        attestation.clearRootKey();

        skip(1);
        j = journalFor(HOLDER_IMAGE, successor, rootLeaf(root));
        vm.expectRevert(AttestationRegistry.RootKeyRetired.selector);
        attestation.setRootKey(sealOf(j), j, root, new bytes32[](0));
        j = journalFor(EXECUTOR_IMAGE, instance, rootLeaf(root));
        vm.expectRevert(AttestationRegistry.RootKeyRetired.selector);
        attestation.setRootKey(sealOf(j), j, root, new bytes32[](0));
        assertEq(attestation.rootKey().signer, address(0));
    }
}
