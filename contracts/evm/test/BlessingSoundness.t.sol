// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {DevAttestationVerifier} from "../src/DevAttestationVerifier.sol";
import {ImageKind, KeyInfo} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// Properties of blessing and checker rotation that were checked and found
/// to hold, kept as regression tests.
contract BlessingSoundnessTest is Fixture {
    DevAttestationVerifier internal second;
    address internal vault = makeAddr("vault");
    bytes32 internal bundle = keccak256("bundle");

    function setUp() public override {
        super.setUp();
        second = new DevAttestationVerifier(attestor);
        vm.prank(admin);
        attestation.proposeVerifier(address(second));
        skip(TIMELOCK);
    }

    /// The "checker no longer allowed" path lets a re-bless move a blessing
    /// whose checker was retired. A stranger holding an OLDER public seal can
    /// use that path only while the blessing is dead; once the operator has
    /// revived it with the newer seal, the older one cannot shorten it.
    function test_an_older_seal_cannot_shorten_a_revived_blessing() public {
        bytes memory old = journalFor(EXECUTOR_IMAGE, executor, leafFor(vault, bundle));
        bytes memory oldSeal = sealOf(old);
        attestation.bless(oldSeal, old, block.chainid, vault, bundle, new bytes32[](0));
        uint64 oldUntil = uint64(vm.getBlockTimestamp()) + BLESSING_TTL;

        skip(1 days);
        bytes memory fresh = journalFor(EXECUTOR_IMAGE, executor, leafFor(vault, bundle));
        bytes memory freshSeal = sealOf(fresh);
        attestation.bless(freshSeal, fresh, block.chainid, vault, bundle, new bytes32[](0));
        uint64 freshUntil = uint64(vm.getBlockTimestamp()) + BLESSING_TTL;
        // Recorded against the newest checker.
        vm.prank(admin);
        attestation.retireVerifier(address(second));
        assertFalse(attestation.isBlessedFor(executor, block.chainid, vault, bundle));

        // The operator revives it through the first checker.
        attestation.bless(freshSeal, fresh, block.chainid, vault, bundle, new bytes32[](0));
        assertEq(attestation.blessedUntil(executor, block.chainid, vault, bundle), freshUntil);

        // A stranger's older seal does nothing to it.
        vm.prank(alice);
        attestation.bless(oldSeal, old, block.chainid, vault, bundle, new bytes32[](0));
        assertEq(attestation.blessedUntil(executor, block.chainid, vault, bundle), freshUntil);
        assertTrue(oldUntil < freshUntil);
        assertTrue(attestation.isBlessedFor(executor, block.chainid, vault, bundle));
    }

    /// Before the revival, a stranger's older seal revives the blessing to
    /// the older expiry. That is never worse than dead, and the operator's
    /// newer seal still wins afterwards.
    function test_a_strangers_revival_with_an_older_seal_is_overtaken_by_the_newer_one() public {
        bytes memory old = journalFor(EXECUTOR_IMAGE, executor, leafFor(vault, bundle));
        bytes memory oldSeal = sealOf(old);
        attestation.bless(oldSeal, old, block.chainid, vault, bundle, new bytes32[](0));
        uint64 oldUntil = uint64(vm.getBlockTimestamp()) + BLESSING_TTL;
        skip(1 days);
        bytes memory fresh = journalFor(EXECUTOR_IMAGE, executor, leafFor(vault, bundle));
        bytes memory freshSeal = sealOf(fresh);
        attestation.bless(freshSeal, fresh, block.chainid, vault, bundle, new bytes32[](0));
        uint64 freshUntil = uint64(vm.getBlockTimestamp()) + BLESSING_TTL;
        vm.prank(admin);
        attestation.retireVerifier(address(second));

        vm.prank(alice);
        attestation.bless(oldSeal, old, block.chainid, vault, bundle, new bytes32[](0));
        assertEq(attestation.blessedUntil(executor, block.chainid, vault, bundle), oldUntil);
        assertTrue(attestation.isBlessedFor(executor, block.chainid, vault, bundle));

        attestation.bless(freshSeal, fresh, block.chainid, vault, bundle, new bytes32[](0));
        assertEq(attestation.blessedUntil(executor, block.chainid, vault, bundle), freshUntil);
    }

    /// `keyInfo`'s vault fields follow the same rule: a stranger replaying
    /// the other leaf of a two-vault journal through the retired-checker path
    /// cannot re-point them away from a later blessing.
    function test_keyinfo_follows_the_latest_blessing_even_through_a_retirement() public {
        address other = makeAddr("other vault");
        bytes32 leafA = leafFor(vault, bundle);
        bytes32 leafB = leafFor(other, bundle);
        bytes32 root = leafA < leafB
            ? keccak256(abi.encodePacked(leafA, leafB))
            : keccak256(abi.encodePacked(leafB, leafA));
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, root);
        bytes memory seal = sealOf(journal);
        bytes32[] memory proofA = new bytes32[](1);
        proofA[0] = leafB;
        bytes32[] memory proofB = new bytes32[](1);
        proofB[0] = leafA;
        attestation.bless(seal, journal, block.chainid, other, bundle, proofB);

        skip(1 days);
        blessExecutorKey(executor, vault, bundle);
        assertEq(attestation.keyInfo(executor).vault, vault);

        vm.prank(admin);
        attestation.retireVerifier(address(second));
        // The operator revives the later blessing through the first checker.
        blessExecutorKey(executor, vault, bundle);
        assertEq(attestation.keyInfo(executor).vault, vault);

        vm.prank(alice);
        attestation.bless(seal, journal, block.chainid, other, bundle, proofB);
        assertEq(attestation.keyInfo(executor).vault, vault, "a stranger re-pointed keyInfo");
    }

    /// A retired checker can never be proposed again, so there is no path by
    /// which every blessing it recorded comes back without a fresh seal.
    function test_a_retired_checker_stays_retired() public {
        vm.startPrank(admin);
        attestation.retireVerifier(address(second));
        vm.expectRevert(AttestationRegistry.AlreadyProposed.selector);
        attestation.proposeVerifier(address(second));
        vm.stopPrank();
        assertFalse(attestation.isVerifierAllowed(address(second)));
    }

    /// The re-proposal path that clears a stale retirement or revocation only
    /// reaches ids never proposed: a revoked image or project, or a retired
    /// checker, cannot be re-proposed, and so cannot come back early.
    function test_revoked_images_and_projects_cannot_be_reproposed() public {
        vm.startPrank(admin);
        attestation.revokeImage(BUILDER_IMAGE);
        vm.expectRevert(AttestationRegistry.AlreadyProposed.selector);
        attestation.proposeImage(BUILDER_IMAGE, ImageKind.Builder);
        attestation.revokeProject(PROJECT);
        vm.expectRevert(AttestationRegistry.AlreadyProposed.selector);
        attestation.proposeProject(PROJECT);
        vm.stopPrank();
        assertFalse(attestation.isBlessedAs(builder, ImageKind.Builder));
    }

    /// A proof the newest checker rejects but an older one accepts is still
    /// blessed, through the older one.
    function test_an_older_checker_still_blesses_what_the_newest_rejects() public {
        uint256 otherAttestor = 0x0DD;
        DevAttestationVerifier third = new DevAttestationVerifier(vm.addr(otherAttestor));
        vm.prank(admin);
        attestation.proposeVerifier(address(third));
        skip(TIMELOCK);
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, leafFor(vault, bundle));
        attestation.bless(sealOf(journal), journal, block.chainid, vault, bundle, new bytes32[](0));
        assertTrue(attestation.isBlessedFor(executor, block.chainid, vault, bundle));
        KeyInfo memory info = attestation.keyInfo(executor);
        assertEq(info.verifier, address(second));
    }
}
