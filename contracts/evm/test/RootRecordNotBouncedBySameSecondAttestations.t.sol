// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {DevAttestationVerifier} from "../src/DevAttestationVerifier.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// The record follows the root to a later attestation. Two allowed images can
/// attest the same root in the same second (an executor and the holder it
/// just handed the root to, or two instances started together), and every
/// `setRootKey` is public calldata anyone may send again. So an attestation
/// from the same second as the one recorded, whatever its image or project,
/// moves the record only to a newer checker, or off one no longer allowed:
/// the two can't be replayed in turn to bounce the record between images, or
/// back onto an older checker. A strictly later attestation moves it as ever.
contract RootRecordNotBouncedBySameSecondAttestationsTest is Fixture {
    bytes32 internal constant NEXT_EXECUTOR_IMAGE = keccak256("the next executor image");
    uint256 internal constant SECOND_ATTESTOR_KEY = 0x5EC0;

    address internal instance = makeAddr("instance key");
    address internal successor = makeAddr("the successor's instance key");
    address internal root = makeAddr("root signing key");

    function rootLeaf(address signer) internal view returns (bytes32) {
        return keyLeaf(0, address(0), attestation.ROOT_KEY_PURPOSE(), signer);
    }

    function setUp() public override {
        super.setUp();
        vm.prank(admin);
        attestation.proposeImage(NEXT_EXECUTOR_IMAGE, ImageKind.Executor);
        skip(TIMELOCK);
    }

    function newerChecker() internal returns (DevAttestationVerifier newer) {
        newer = new DevAttestationVerifier(vm.addr(SECOND_ATTESTOR_KEY));
        vm.prank(admin);
        attestation.proposeVerifier(address(newer));
        skip(TIMELOCK);
    }

    /// Through the same checker, another image's attestation from the same
    /// second moves nothing, in either order; one a second later moves it.
    function test_a_same_second_attestation_from_another_image_does_not_move_the_record() public {
        bytes memory first = journalFor(EXECUTOR_IMAGE, instance, rootLeaf(root));
        bytes memory second = journalFor(NEXT_EXECUTOR_IMAGE, successor, rootLeaf(root));
        attestation.setRootKey(sealOf(first), first, root, new bytes32[](0));

        vm.expectRevert(AttestationRegistry.RootKeyAlreadySet.selector);
        attestation.setRootKey(sealOf(second), second, root, new bytes32[](0));
        assertEq(attestation.rootKey().imageDigest, EXECUTOR_IMAGE);

        skip(1);
        bytes memory later = journalFor(NEXT_EXECUTOR_IMAGE, successor, rootLeaf(root));
        attestation.setRootKey(sealOf(later), later, root, new bytes32[](0));
        assertEq(attestation.rootKey().imageDigest, NEXT_EXECUTOR_IMAGE);
    }

    /// Another image's attestation from the same second moves the record onto
    /// a newer checker; replaying the first, through the older checker, does
    /// not move it back.
    function test_a_same_second_replay_does_not_move_the_record_back_to_an_older_checker() public {
        DevAttestationVerifier newer = newerChecker();
        bytes memory first = journalFor(EXECUTOR_IMAGE, instance, rootLeaf(root));
        bytes memory second = journalFor(NEXT_EXECUTOR_IMAGE, successor, rootLeaf(root));
        attestation.setRootKey(sealOf(first), first, root, new bytes32[](0));
        assertEq(attestation.rootKey().verifier, address(verifier));
        attestation.setRootKey(signed(SECOND_ATTESTOR_KEY, keccak256(second)), second, root, new bytes32[](0));
        assertEq(attestation.rootKey().verifier, address(newer));
        assertEq(attestation.rootKey().imageDigest, NEXT_EXECUTOR_IMAGE);

        vm.expectRevert(AttestationRegistry.RootKeyAlreadySet.selector);
        attestation.setRootKey(sealOf(first), first, root, new bytes32[](0));
        assertEq(attestation.rootKey().verifier, address(newer));
        assertEq(attestation.rootKey().imageDigest, NEXT_EXECUTOR_IMAGE);
    }

    /// Off a checker no longer allowed, an attestation from the same second
    /// moves the record, whatever its image.
    function test_a_same_second_attestation_moves_the_record_off_a_retired_checker() public {
        DevAttestationVerifier newer = newerChecker();
        bytes memory first = journalFor(EXECUTOR_IMAGE, instance, rootLeaf(root));
        bytes memory second = journalFor(NEXT_EXECUTOR_IMAGE, successor, rootLeaf(root));
        attestation.setRootKey(sealOf(first), first, root, new bytes32[](0));
        vm.prank(admin);
        attestation.retireVerifier(address(verifier));
        assertFalse(attestation.isRootKeyLive());

        attestation.setRootKey(signed(SECOND_ATTESTOR_KEY, keccak256(second)), second, root, new bytes32[](0));
        assertEq(attestation.rootKey().verifier, address(newer));
        assertEq(attestation.rootKey().imageDigest, NEXT_EXECUTOR_IMAGE);
        assertTrue(attestation.isRootKeyLive());
    }
}
