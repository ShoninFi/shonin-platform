// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {DevAttestationVerifier} from "../src/DevAttestationVerifier.sol";
import {IAttestationRegistry, ImageKind, RootKeyRecord} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// The registry records the root key's public half once, from an attested
/// executor or holder that committed to it among its leaves, and only the
/// admin can clear it. The same root follows a later attestation from any
/// allowed executor or holder image, and a checker rotation as a blessing
/// does; a cleared root is never recorded again. The record is live
/// only while the image, the project and the checker that last recorded it
/// are all still allowed.
///
/// Each test states what holds.
contract RootKeyAttestationTest is Fixture {
    bytes32 internal constant HOLDER_IMAGE = keccak256("holder image");
    bytes32 internal constant UNWINDER_IMAGE = keccak256("unwinder image");
    uint256 internal constant SECOND_ATTESTOR_KEY = 0x5EC0;

    address internal instance = makeAddr("instance key");
    address internal root = makeAddr("root signing key");
    address internal otherInstance = makeAddr("another instance");
    address internal vaultKey = makeAddr("key for a vault");
    address internal vault = makeAddr("a vault");
    bytes32 internal bundle = keccak256("bundle");

    /// An executor's two leaves: a key for one vault, and the root.
    bytes32[2] internal leaves;
    bytes internal journal;
    bytes internal seal;

    function setUp() public override {
        super.setUp();
        vm.startPrank(admin);
        attestation.proposeImage(HOLDER_IMAGE, ImageKind.Holder);
        attestation.proposeImage(UNWINDER_IMAGE, ImageKind.Unwinder);
        vm.stopPrank();
        skip(TIMELOCK);

        leaves[0] = leafFor(vault, bundle, vaultKey);
        leaves[1] = rootLeaf(root);
        journal = journalFor(EXECUTOR_IMAGE, instance, rootOf(leaves));
        seal = sealOf(journal);
    }

    /// The leaf an instance commits to for the root's signing key, on this
    /// chain.
    function rootLeaf(address signer) internal view returns (bytes32) {
        return keyLeaf(0, address(0), attestation.ROOT_KEY_PURPOSE(), signer);
    }

    function pair(bytes32 a, bytes32 b) internal pure returns (bytes32) {
        return a < b ? keccak256(abi.encodePacked(a, b)) : keccak256(abi.encodePacked(b, a));
    }

    function rootOf(bytes32[2] memory l) internal pure returns (bytes32) {
        return pair(l[0], l[1]);
    }

    function proofOf(bytes32[2] memory l, uint256 index) internal pure returns (bytes32[] memory proof) {
        proof = new bytes32[](1);
        proof[0] = l[1 - index];
    }

    function setRoot() internal {
        attestation.setRootKey(seal, journal, root, proofOf(leaves, 1));
    }

    /// A second checker, proposed after the first and allowed once its
    /// timelock passes, that accepts what `SECOND_ATTESTOR_KEY` signs.
    function newerChecker() internal returns (DevAttestationVerifier second) {
        second = new DevAttestationVerifier(vm.addr(SECOND_ATTESTOR_KEY));
        vm.prank(admin);
        attestation.proposeVerifier(address(second));
        skip(TIMELOCK);
    }

    /// A newer checker that accepts exactly the proofs the first one does,
    /// as a rotation's successor with the same trust root would.
    function sameRootChecker() internal returns (DevAttestationVerifier second) {
        second = new DevAttestationVerifier(attestor);
        vm.prank(admin);
        attestation.proposeVerifier(address(second));
        skip(TIMELOCK);
    }

    /// An instance of `image` that commits to `signer` as the root and to
    /// nothing else, attested now.
    function rootOnly(bytes32 image, address signer) internal view returns (bytes memory j, bytes memory s) {
        j = journalFor(image, otherInstance, rootLeaf(signer));
        s = sealOf(j);
    }

    // Setting ---------------------------------------------------------------

    function test_a_valid_proof_from_an_executor_image_sets_the_root_key() public {
        vm.expectEmit(address(attestation));
        emit IAttestationRegistry.RootKeySet(
            root, EXECUTOR_IMAGE, PROJECT, address(verifier), uint64(block.timestamp)
        );
        vm.prank(alice);
        setRoot();

        RootKeyRecord memory record = attestation.rootKey();
        assertEq(record.signer, root);
        assertEq(record.imageDigest, EXECUTOR_IMAGE);
        assertEq(record.projectId, PROJECT);
        assertEq(record.verifier, address(verifier));
        assertEq(record.issuedAt, uint64(block.timestamp));
        assertEq(record.setAt, uint64(block.timestamp));
        assertTrue(attestation.isRootKeyLive());

        // The same attestation still blesses the vault key beside it.
        attestation.bless(seal, journal, block.chainid, vault, bundle, vaultKey, proofOf(leaves, 0));
        assertTrue(attestation.isBlessedFor(vaultKey, block.chainid, vault, bundle));
        // And the root's signer is blessed for nothing.
        assertTrue(attestation.keyInfo(root).kind == ImageKind.None);
    }

    function test_a_valid_proof_from_a_holder_image_sets_the_root_key() public {
        (bytes memory j, bytes memory s) = rootOnly(HOLDER_IMAGE, root);
        attestation.setRootKey(s, j, root, new bytes32[](0));
        assertEq(attestation.rootKey().signer, root);
        assertEq(attestation.rootKey().imageDigest, HOLDER_IMAGE);
        assertTrue(attestation.isRootKeyLive());
    }

    /// The vector the services' Rust checks (`chain::attest::root_leaf`): on
    /// chain 31337, the root leaf for the signer 0xe1e1…e1 is this. A journal
    /// whose root is that leaf alone sets that signer, so the registry's own
    /// leaf is the same bytes.
    function test_the_root_leaf_for_fixed_inputs_is_the_off_chain_vector() public {
        bytes32 vector = 0xfb5b63d20acbdca3915f1a92aee1020f9b765f3f37160cf0599ec25861db59c4;
        address signer = address(0xE1E1E1e1E1E1e1e1E1E1E1e1e1e1E1E1E1E1e1e1);
        vm.chainId(31_337);
        assertEq(attestation.ROOT_KEY_PURPOSE(), keccak256("shonin:root-key:v1"));
        assertEq(rootLeaf(signer), vector);

        bytes memory j = journalFor(EXECUTOR_IMAGE, otherInstance, vector);
        attestation.setRootKey(sealOf(j), j, signer, new bytes32[](0));
        assertEq(attestation.rootKey().signer, signer);
    }

    function test_a_builder_reviewer_or_unwinder_image_cannot_set_the_root_key() public {
        bytes32[3] memory images = [BUILDER_IMAGE, REVIEWER_IMAGE, UNWINDER_IMAGE];
        for (uint256 i = 0; i < images.length; i++) {
            (bytes memory j, bytes memory s) = rootOnly(images[i], root);
            vm.expectRevert(AttestationRegistry.WrongKindForRoot.selector);
            attestation.setRootKey(s, j, root, new bytes32[](0));
        }
        assertEq(attestation.rootKey().signer, address(0));
        assertFalse(attestation.isRootKeyLive());
    }

    function test_an_unallowed_project_cannot_set_the_root_key() public {
        bytes memory elsewhere = abi.encode(
            AttestationRegistry.Journal({
                imageDigest: EXECUTOR_IMAGE,
                projectId: keccak256("a project nobody proposed"),
                signer: instance,
                vaultsRoot: rootLeaf(root),
                issuedAt: uint64(block.timestamp)
            })
        );
        vm.expectRevert(AttestationRegistry.ProjectNotAllowed.selector);
        attestation.setRootKey(sealOf(elsewhere), elsewhere, root, new bytes32[](0));

        // Nor one that was allowed and has been revoked.
        vm.prank(admin);
        attestation.revokeProject(PROJECT);
        vm.expectRevert(AttestationRegistry.ProjectNotAllowed.selector);
        setRoot();
        assertEq(attestation.rootKey().signer, address(0));
    }

    function test_an_unallowed_image_cannot_set_the_root_key() public {
        (bytes memory j, bytes memory s) = rootOnly(keccak256("an image nobody proposed"), root);
        vm.expectRevert(AttestationRegistry.ImageNotAllowed.selector);
        attestation.setRootKey(s, j, root, new bytes32[](0));
    }

    function test_a_retired_checker_cannot_set_the_root_key() public {
        DevAttestationVerifier second = new DevAttestationVerifier(vm.addr(SECOND_ATTESTOR_KEY));
        vm.prank(admin);
        attestation.proposeVerifier(address(second));
        skip(TIMELOCK);
        journal = journalFor(EXECUTOR_IMAGE, instance, rootOf(leaves));
        bytes memory secondSeal = signed(SECOND_ATTESTOR_KEY, keccak256(journal));

        vm.prank(admin);
        attestation.retireVerifier(address(second));
        vm.expectRevert(AttestationRegistry.ProofRejected.selector);
        attestation.setRootKey(secondSeal, journal, root, proofOf(leaves, 1));

        // The first checker, retired, refuses its own seal as well.
        vm.prank(admin);
        attestation.retireVerifier(address(verifier));
        vm.expectRevert(AttestationRegistry.ProofRejected.selector);
        attestation.setRootKey(sealOf(journal), journal, root, proofOf(leaves, 1));
        assertEq(attestation.rootKey().signer, address(0));
    }

    function test_a_stale_or_future_token_cannot_set_the_root_key() public {
        skip(BLESSING_TTL);
        vm.expectRevert(AttestationRegistry.Stale.selector);
        setRoot();

        bytes memory future = abi.encode(
            AttestationRegistry.Journal({
                imageDigest: EXECUTOR_IMAGE,
                projectId: PROJECT,
                signer: instance,
                vaultsRoot: rootLeaf(root),
                issuedAt: uint64(block.timestamp) + 1
            })
        );
        vm.expectRevert(AttestationRegistry.Stale.selector);
        attestation.setRootKey(sealOf(future), future, root, new bytes32[](0));
        assertEq(attestation.rootKey().signer, address(0));
    }

    function test_a_root_leaf_not_under_the_journal_s_root_is_refused() public {
        // Another signer than the one committed to.
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.setRootKey(seal, journal, alice, proofOf(leaves, 1));
        // The right signer with the wrong proof.
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.setRootKey(seal, journal, root, proofOf(leaves, 0));
        // The instance's own key, which names it and is no root.
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.setRootKey(seal, journal, instance, proofOf(leaves, 1));
        // The zero signer, whatever the proof.
        vm.expectRevert(AttestationRegistry.NoSigner.selector);
        attestation.setRootKey(seal, journal, address(0), proofOf(leaves, 1));
        // The leaf names the chain it is sent on: on another chain it is not
        // among the leaves.
        uint256 here = block.chainid;
        vm.chainId(here + 1);
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        setRoot();
        vm.chainId(here);
        assertEq(attestation.rootKey().signer, address(0));
    }

    function test_a_second_set_is_refused_while_a_root_key_is_recorded() public {
        setRoot();
        // Neither the same proof again...
        vm.expectRevert(AttestationRegistry.RootKeyAlreadySet.selector);
        setRoot();
        // ...nor a fresh one, from a holder, for another root.
        skip(1);
        address other = makeAddr("another root");
        (bytes memory j, bytes memory s) = rootOnly(HOLDER_IMAGE, other);
        vm.expectRevert(AttestationRegistry.RootKeyAlreadySet.selector);
        attestation.setRootKey(s, j, other, new bytes32[](0));
        assertEq(attestation.rootKey().signer, root);
    }

    // Leaves ----------------------------------------------------------------

    function test_a_root_leaf_cannot_bless_a_key() public {
        bytes32 purpose = attestation.ROOT_KEY_PURPOSE();
        // An executor, as the root leaf is shaped: no vault.
        vm.expectRevert(AttestationRegistry.WrongKindForVault.selector);
        attestation.bless(seal, journal, 0, address(0), purpose, root, proofOf(leaves, 1));
        // Nor with the purpose as a bundle for a real vault.
        vm.expectRevert(AttestationRegistry.WrongKindForVault.selector);
        attestation.bless(seal, journal, block.chainid, vault, purpose, root, proofOf(leaves, 1));
        // A builder carrying a root leaf can't bless it either.
        (bytes memory j, bytes memory s) = rootOnly(BUILDER_IMAGE, root);
        vm.expectRevert(AttestationRegistry.WrongKindForVault.selector);
        attestation.bless(s, j, 0, address(0), purpose, root, new bytes32[](0));
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.bless(s, j, 0, address(0), bytes32(0), root, new bytes32[](0));
        // Nor a holder, which blesses nothing at all.
        (j, s) = rootOnly(HOLDER_IMAGE, root);
        vm.expectRevert(AttestationRegistry.HolderBlessesNothing.selector);
        attestation.bless(s, j, 0, address(0), purpose, root, new bytes32[](0));
        vm.expectRevert(AttestationRegistry.HolderBlessesNothing.selector);
        attestation.bless(s, j, 0, address(0), bytes32(0), root, new bytes32[](0));
        assertTrue(attestation.keyInfo(root).kind == ImageKind.None);
    }

    function test_a_blessing_leaf_cannot_set_the_root_key() public {
        // The vault key's leaf, sent as the root.
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.setRootKey(seal, journal, vaultKey, proofOf(leaves, 0));
        // A holder that committed to its key's leaf with no vault, not to a
        // root leaf.
        address holderKey = makeAddr("holder's own key");
        bytes memory j = journalFor(HOLDER_IMAGE, instance, serviceLeaf(holderKey));
        bytes memory s = sealOf(j);
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.setRootKey(s, j, holderKey, new bytes32[](0));
        assertEq(attestation.rootKey().signer, address(0));
    }

    // Clearing --------------------------------------------------------------

    function test_only_the_admin_clears_the_root_key() public {
        setRoot();
        vm.prank(alice);
        vm.expectRevert(AttestationRegistry.NotAdmin.selector);
        attestation.clearRootKey();
        assertEq(attestation.rootKey().signer, root);

        vm.expectEmit(address(attestation));
        emit IAttestationRegistry.RootKeyCleared(root);
        vm.prank(admin);
        attestation.clearRootKey();
        assertEq(attestation.rootKey().signer, address(0));
        assertFalse(attestation.isRootKeyLive());

        // There is nothing left to clear.
        vm.prank(admin);
        vm.expectRevert(AttestationRegistry.NoRootKey.selector);
        attestation.clearRootKey();
    }

    function test_the_admin_cannot_set_the_root_key() public {
        // Without a proof from an allowed checker, the admin is anyone.
        bytes memory forged = signed(0xAD, keccak256(journal));
        vm.prank(admin);
        vm.expectRevert(AttestationRegistry.ProofRejected.selector);
        attestation.setRootKey(forged, journal, root, proofOf(leaves, 1));
        // Nor can the admin claim a root no instance committed to.
        vm.prank(admin);
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        attestation.setRootKey(seal, journal, admin, proofOf(leaves, 1));
        assertEq(attestation.rootKey().signer, address(0));
    }

    function test_after_a_clear_a_new_proof_sets_a_new_root_key() public {
        setRoot();
        skip(1 hours);
        vm.prank(admin);
        attestation.clearRootKey();

        skip(1);
        address next = makeAddr("the next root");
        (bytes memory j, bytes memory s) = rootOnly(HOLDER_IMAGE, next);
        attestation.setRootKey(s, j, next, new bytes32[](0));
        RootKeyRecord memory record = attestation.rootKey();
        assertEq(record.signer, next);
        assertEq(record.imageDigest, HOLDER_IMAGE);
        assertTrue(attestation.isRootKeyLive());
    }

    function test_a_proof_made_before_a_clear_cannot_set_a_root_key() public {
        setRoot();
        // A proof for another root, made before the clear and still within
        // its TTL afterwards.
        address next = makeAddr("the next root");
        (bytes memory early, bytes memory earlySeal) = rootOnly(HOLDER_IMAGE, next);
        skip(1 hours);
        vm.prank(admin);
        attestation.clearRootKey();

        // Replaying the cleared root's own proof can't put it back...
        vm.expectRevert(AttestationRegistry.RootKeyRetired.selector);
        setRoot();
        // ...and a proof made before the clear can't set any other root.
        vm.expectRevert(AttestationRegistry.Stale.selector);
        attestation.setRootKey(earlySeal, early, next, new bytes32[](0));
        assertEq(attestation.rootKey().signer, address(0));
    }

    function test_a_cleared_root_key_is_refused_for_good() public {
        setRoot();
        skip(1 hours);
        vm.prank(admin);
        attestation.clearRootKey();

        // An instance still allowed to run, still holding the cleared root,
        // attests afresh and commits to it again: refused.
        skip(1);
        (bytes memory j, bytes memory s) = rootOnly(EXECUTOR_IMAGE, root);
        vm.expectRevert(AttestationRegistry.RootKeyRetired.selector);
        attestation.setRootKey(s, j, root, new bytes32[](0));
        // From a holder, too.
        (j, s) = rootOnly(HOLDER_IMAGE, root);
        vm.expectRevert(AttestationRegistry.RootKeyRetired.selector);
        attestation.setRootKey(s, j, root, new bytes32[](0));

        // Still refused after the next root has come and gone, long after,
        // through a checker that never saw it.
        address next = makeAddr("the next root");
        (j, s) = rootOnly(HOLDER_IMAGE, next);
        attestation.setRootKey(s, j, next, new bytes32[](0));
        skip(1 hours);
        vm.prank(admin);
        attestation.clearRootKey();
        newerChecker();
        skip(BLESSING_TTL * 10);
        j = journalFor(HOLDER_IMAGE, otherInstance, rootLeaf(root));
        s = signed(SECOND_ATTESTOR_KEY, keccak256(j));
        vm.expectRevert(AttestationRegistry.RootKeyRetired.selector);
        attestation.setRootKey(s, j, root, new bytes32[](0));
        // Every cleared root stays out.
        (j, s) = rootOnly(HOLDER_IMAGE, next);
        vm.expectRevert(AttestationRegistry.RootKeyRetired.selector);
        attestation.setRootKey(s, j, next, new bytes32[](0));
        assertEq(attestation.rootKey().signer, address(0));
    }

    // Checker rotation ------------------------------------------------------

    function test_the_same_proof_relayed_in_a_rotation_s_overlap_moves_the_root_to_the_newer_checker()
        public
    {
        setRoot();
        attestation.bless(seal, journal, block.chainid, vault, bundle, vaultKey, proofOf(leaves, 0));
        assertEq(attestation.rootKey().verifier, address(verifier));
        uint64 issuedAt = attestation.rootKey().issuedAt;

        DevAttestationVerifier second = sameRootChecker();
        assertTrue(second.verify(seal, journal));

        // The overlap: the same proof relayed again, as a blessing is.
        attestation.bless(seal, journal, block.chainid, vault, bundle, vaultKey, proofOf(leaves, 0));
        vm.expectEmit(address(attestation));
        emit IAttestationRegistry.RootKeySet(root, EXECUTOR_IMAGE, PROJECT, address(second), issuedAt);
        vm.prank(alice);
        setRoot();
        RootKeyRecord memory record = attestation.rootKey();
        assertEq(record.signer, root);
        assertEq(record.verifier, address(second));
        assertEq(record.issuedAt, issuedAt);
        assertEq(record.setAt, uint64(block.timestamp));

        // The overlap ends: the blessing and the root both outlive the old
        // checker.
        vm.prank(admin);
        attestation.retireVerifier(address(verifier));
        assertTrue(attestation.isBlessedFor(vaultKey, block.chainid, vault, bundle));
        assertTrue(attestation.isRootKeyLive());

        // Once moved, relaying it again changes nothing.
        vm.expectRevert(AttestationRegistry.RootKeyAlreadySet.selector);
        setRoot();
    }

    function test_a_fresh_attestation_in_a_rotation_s_overlap_moves_the_root_to_the_newer_checker() public {
        setRoot();
        newerChecker();

        // A holder that received the same root attests through the newer
        // checker only.
        bytes memory j = journalFor(HOLDER_IMAGE, otherInstance, rootLeaf(root));
        bytes memory s = signed(SECOND_ATTESTOR_KEY, keccak256(j));
        attestation.setRootKey(s, j, root, new bytes32[](0));
        RootKeyRecord memory record = attestation.rootKey();
        address second = attestation.verifiers()[1];
        assertEq(record.signer, root);
        assertEq(record.verifier, second);
        assertEq(record.imageDigest, HOLDER_IMAGE);
        assertEq(record.issuedAt, uint64(block.timestamp));

        vm.prank(admin);
        attestation.retireVerifier(address(verifier));
        assertTrue(attestation.isRootKeyLive());
    }

    function test_another_root_is_refused_during_a_rotation_s_overlap() public {
        setRoot();
        newerChecker();

        address other = makeAddr("another root");
        bytes memory j = journalFor(HOLDER_IMAGE, otherInstance, rootLeaf(other));
        bytes memory s = signed(SECOND_ATTESTOR_KEY, keccak256(j));
        vm.expectRevert(AttestationRegistry.RootKeyAlreadySet.selector);
        attestation.setRootKey(s, j, other, new bytes32[](0));
        RootKeyRecord memory record = attestation.rootKey();
        assertEq(record.signer, root);
        assertEq(record.verifier, address(verifier));
    }

    function test_the_same_attestation_through_an_older_checker_cannot_move_the_root_back() public {
        setRoot();
        newerChecker();
        bytes memory j = journalFor(EXECUTOR_IMAGE, instance, rootOf(leaves));
        attestation.setRootKey(signed(SECOND_ATTESTOR_KEY, keccak256(j)), j, root, proofOf(leaves, 1));
        address second = attestation.verifiers()[1];
        assertEq(attestation.rootKey().verifier, second);

        // The same attestation, as only the first checker accepts it.
        vm.expectRevert(AttestationRegistry.RootKeyAlreadySet.selector);
        attestation.setRootKey(sealOf(j), j, root, proofOf(leaves, 1));
        assertEq(attestation.rootKey().verifier, second);

        // A later attestation of the same root is recorded through whichever
        // allowed checker accepts it, as any later attestation is.
        skip(1);
        j = journalFor(EXECUTOR_IMAGE, instance, rootOf(leaves));
        attestation.setRootKey(sealOf(j), j, root, proofOf(leaves, 1));
        assertEq(attestation.rootKey().verifier, address(verifier));
        assertEq(attestation.rootKey().issuedAt, uint64(block.timestamp));
    }

    function test_the_root_follows_a_retired_newest_checker_back_to_an_older_allowed_one_when_reattested()
        public
    {
        // Recorded through the newer checker, which is then retired.
        newerChecker();
        bytes memory j = journalFor(EXECUTOR_IMAGE, instance, rootOf(leaves));
        attestation.setRootKey(signed(SECOND_ATTESTOR_KEY, keccak256(j)), j, root, proofOf(leaves, 1));
        address second = attestation.verifiers()[1];
        vm.prank(admin);
        attestation.retireVerifier(second);
        assertFalse(attestation.isRootKeyLive());

        // An attestation earlier than the recorded one can't revive it.
        bytes memory early = journal;
        vm.expectRevert(AttestationRegistry.RootKeyAlreadySet.selector);
        attestation.setRootKey(sealOf(early), early, root, proofOf(leaves, 1));

        // Nor can another root, through the older checker.
        skip(1);
        address other = makeAddr("another root");
        (bytes memory oj, bytes memory os) = rootOnly(HOLDER_IMAGE, other);
        vm.expectRevert(AttestationRegistry.RootKeyAlreadySet.selector);
        attestation.setRootKey(os, oj, other, new bytes32[](0));

        // A fresh attestation of the same root, through the older checker
        // still allowed, records it there and makes it live again.
        j = journalFor(EXECUTOR_IMAGE, instance, rootOf(leaves));
        attestation.setRootKey(sealOf(j), j, root, proofOf(leaves, 1));
        RootKeyRecord memory record = attestation.rootKey();
        assertEq(record.signer, root);
        assertEq(record.verifier, address(verifier));
        assertEq(record.issuedAt, uint64(block.timestamp));
        assertTrue(attestation.isRootKeyLive());

        // A cleared root stays out, whichever checker is allowed.
        vm.prank(admin);
        attestation.clearRootKey();
        skip(1);
        j = journalFor(EXECUTOR_IMAGE, instance, rootOf(leaves));
        vm.expectRevert(AttestationRegistry.RootKeyRetired.selector);
        attestation.setRootKey(sealOf(j), j, root, proofOf(leaves, 1));
    }

    function test_an_earlier_attestation_cannot_move_the_root() public {
        // An attestation made now, recorded a minute later from a later one.
        bytes memory early = journalFor(EXECUTOR_IMAGE, instance, rootOf(leaves));
        skip(1 minutes);
        journal = journalFor(EXECUTOR_IMAGE, instance, rootOf(leaves));
        seal = sealOf(journal);
        setRoot();
        newerChecker();

        // The newer checker accepts the earlier one, which can't move it.
        vm.expectRevert(AttestationRegistry.RootKeyAlreadySet.selector);
        attestation.setRootKey(signed(SECOND_ATTESTOR_KEY, keccak256(early)), early, root, proofOf(leaves, 1));
        assertEq(attestation.rootKey().verifier, address(verifier));
    }

    // Liveness --------------------------------------------------------------

    function test_revoking_the_image_makes_the_root_key_not_live() public {
        setRoot();
        vm.prank(admin);
        attestation.revokeImage(EXECUTOR_IMAGE);
        assertFalse(attestation.isRootKeyLive());
        // The record stays, so everyone can see which root lost its trust.
        assertEq(attestation.rootKey().signer, root);
    }

    function test_revoking_the_project_makes_the_root_key_not_live() public {
        setRoot();
        vm.prank(admin);
        attestation.revokeProject(PROJECT);
        assertFalse(attestation.isRootKeyLive());
    }

    function test_retiring_the_checker_makes_the_root_key_not_live() public {
        setRoot();
        vm.prank(admin);
        attestation.retireVerifier(address(verifier));
        assertFalse(attestation.isRootKeyLive());
        assertEq(attestation.rootKey().verifier, address(verifier));
    }

    function test_revoking_another_image_that_held_the_root_leaves_it_live_until_cleared() public {
        // The root is recorded from the executor, then moved through a newer
        // checker by a holder that received it.
        setRoot();
        newerChecker();
        bytes memory j = journalFor(HOLDER_IMAGE, otherInstance, rootLeaf(root));
        bytes memory s = signed(SECOND_ATTESTOR_KEY, keccak256(j));
        attestation.setRootKey(s, j, root, new bytes32[](0));
        assertEq(attestation.rootKey().imageDigest, HOLDER_IMAGE);

        // The record knows only the image that last recorded it: revoking
        // the executor image, which also held the root, leaves it live.
        vm.prank(admin);
        attestation.revokeImage(EXECUTOR_IMAGE);
        assertTrue(attestation.isRootKeyLive());

        // A revocation that could have exposed the root is followed by a
        // clear, which ends it for good, even from the holder still running.
        vm.prank(admin);
        attestation.clearRootKey();
        assertFalse(attestation.isRootKeyLive());
        skip(1);
        j = journalFor(HOLDER_IMAGE, otherInstance, rootLeaf(root));
        s = signed(SECOND_ATTESTOR_KEY, keccak256(j));
        vm.expectRevert(AttestationRegistry.RootKeyRetired.selector);
        attestation.setRootKey(s, j, root, new bytes32[](0));
    }

    function test_the_root_key_stays_live_past_the_attestation_s_ttl() public {
        // The record is the root's, not a blessing: it lasts until cleared
        // or until what vouched for it is revoked.
        setRoot();
        skip(BLESSING_TTL * 10);
        assertTrue(attestation.isRootKeyLive());
    }
}
