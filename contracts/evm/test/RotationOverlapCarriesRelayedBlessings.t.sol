// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {DevAttestationVerifier} from "../src/DevAttestationVerifier.sol";
import {KeyInfo} from "../src/interfaces/IAttestationRegistry.sol";

import {Fixture} from "./Fixture.sol";
import {MessageHashUtils} from "openzeppelin/utils/cryptography/MessageHashUtils.sol";

/// A blessing given before a checker rotation, relayed again during the
/// overlap and accepted by the newer checker, is recorded against that
/// checker, so the old checker's retirement doesn't drop it. Moving it never
/// changes how long it lasts, never moves it to an older checker, and never
/// changes which vault `keyInfo` reports.
contract RotationOverlapCarriesRelayedBlessingsTest is Fixture {
    DevAttestationVerifier internal second;
    address internal vault = makeAddr("vault");
    bytes32 internal bundle = keccak256("bundle");

    function setUp() public override {
        super.setUp();
        // The successor: the same trust root, accepting exactly the proofs
        // the first checker accepts.
        second = new DevAttestationVerifier(attestor);
        vm.prank(admin);
        attestation.proposeVerifier(address(second));
    }

    function blessOnce(bytes memory seal, bytes memory journal) internal {
        attestation.bless(seal, journal, block.chainid, vault, bundle, executor, new bytes32[](0));
    }

    function test_a_seal_relayed_again_during_the_overlap_survives_the_old_checkers_retirement() public {
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, leafFor(vault, bundle, executor));
        bytes memory seal = sealOf(journal);
        // Blessed while only the first checker counts.
        blessOnce(seal, journal);
        uint64 until = attestation.blessedUntil(executor, block.chainid, vault, bundle);
        assertEq(attestation.keyInfo(executor).verifier, address(verifier));

        // The overlap: both checkers allowed, both accept this proof.
        skip(TIMELOCK);
        assertTrue(attestation.isVerifierAllowed(address(second)));
        assertTrue(second.verify(seal, journal));

        // The same proof, relayed again.
        vm.prank(alice);
        blessOnce(seal, journal);
        assertEq(
            attestation.keyInfo(executor).verifier, address(second), "keyInfo not moved to the newer checker"
        );
        assertEq(
            attestation.blessedUntil(executor, block.chainid, vault, bundle),
            until,
            "moving it changed how long it lasts"
        );
        assertEq(attestation.keyInfo(executor).blessedUntil, until);

        // The overlap ends.
        vm.prank(admin);
        attestation.retireVerifier(address(verifier));
        assertTrue(
            attestation.isBlessedFor(executor, block.chainid, vault, bundle),
            "a key relayed through the overlap was dropped by the old checker's retirement"
        );
    }

    function test_an_earlier_attestation_through_a_newer_checker_moves_nothing() public {
        bytes memory early = journalFor(EXECUTOR_IMAGE, executor, leafFor(vault, bundle, executor));
        skip(1 hours);
        bytes memory later = journalFor(EXECUTOR_IMAGE, executor, leafFor(vault, bundle, executor));
        blessOnce(sealOf(later), later);
        uint64 until = attestation.blessedUntil(executor, block.chainid, vault, bundle);

        skip(TIMELOCK);
        // The earlier proof, accepted by the newer checker: it would shorten
        // the blessing, so it changes nothing.
        vm.prank(alice);
        blessOnce(sealOf(early), early);
        assertEq(attestation.blessedUntil(executor, block.chainid, vault, bundle), until);
        assertEq(attestation.keyInfo(executor).blessedUntil, until);
        assertEq(attestation.keyInfo(executor).verifier, address(verifier));
    }

    function test_a_blessing_is_never_moved_to_an_older_checker() public {
        // A newer checker trusting another key, so each checker accepts only
        // its own proofs.
        uint256 otherKey = 0xB0B;
        DevAttestationVerifier third = new DevAttestationVerifier(vm.addr(otherKey));
        vm.prank(admin);
        attestation.proposeVerifier(address(third));
        skip(TIMELOCK);

        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, leafFor(vault, bundle, executor));
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(otherKey, MessageHashUtils.toEthSignedMessageHash(keccak256(journal)));
        blessOnce(abi.encodePacked(r, s, v), journal);
        assertEq(attestation.keyInfo(executor).verifier, address(third));

        // The same attestation, accepted only by the older checkers.
        vm.prank(alice);
        blessOnce(sealOf(journal), journal);
        assertEq(attestation.keyInfo(executor).verifier, address(third), "moved to an older checker");

        // So retiring the older checkers leaves it standing.
        vm.startPrank(admin);
        attestation.retireVerifier(address(verifier));
        attestation.retireVerifier(address(second));
        vm.stopPrank();
        assertTrue(attestation.isBlessedFor(executor, block.chainid, vault, bundle));
    }

    function test_relaying_another_vaults_leaf_through_a_newer_checker_keeps_what_keyInfo_reports() public {
        address other = makeAddr("other vault");
        bytes32 leafA = leafFor(vault, bundle, executor);
        bytes32 leafB = leafFor(other, bundle, executor);
        bytes32 root = leafA < leafB
            ? keccak256(abi.encodePacked(leafA, leafB))
            : keccak256(abi.encodePacked(leafB, leafA));
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, root);
        bytes memory seal = sealOf(journal);
        bytes32[] memory proofA = new bytes32[](1);
        proofA[0] = leafB;
        bytes32[] memory proofB = new bytes32[](1);
        proofB[0] = leafA;
        attestation.bless(seal, journal, block.chainid, vault, bundle, executor, proofA);

        skip(TIMELOCK);
        vm.prank(alice);
        attestation.bless(seal, journal, block.chainid, other, bundle, executor, proofB);
        KeyInfo memory info = attestation.keyInfo(executor);
        assertEq(info.vault, vault, "keyInfo re-pointed to another vault");
        assertEq(info.verifier, address(second));
    }
}
