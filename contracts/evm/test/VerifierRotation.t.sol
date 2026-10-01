// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {DevAttestationVerifier} from "../src/DevAttestationVerifier.sol";

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// Rotating the attestation checker works with an overlap: a blessing is
/// recorded against the newest allowed checker that accepts it, so retiring
/// the old checker at the end of the overlap drops no honest key.
contract VerifierRotationTest is Fixture {
    DevAttestationVerifier internal fixedChecker;

    function setUp() public override {
        super.setUp();
        // The same trust root, re-implemented: the fix of the first checker,
        // accepting exactly the proofs it accepts.
        fixedChecker = new DevAttestationVerifier(attestor);
        vm.prank(admin);
        attestation.proposeVerifier(address(fixedChecker));
        skip(TIMELOCK);
        assertTrue(attestation.isVerifierAllowed(address(fixedChecker)));
    }

    /// The rotation as designed: the new checker is ready, instances
    /// keep attesting through the overlap, then the old checker is retired.
    function test_retiring_the_old_checker_after_the_overlap_keeps_keys_the_new_one_accepts() public {
        (Vault vault, bytes32 bundle) = openVault();
        Station station = stationOf(vault);
        // Attested during the overlap, while both checkers are allowed.
        blessExecutorKey(executor, address(vault), bundle);
        assertTrue(attestation.isBlessedFor(executor, block.chainid, address(vault), bundle));
        station.approveKey("local", executor);

        // The overlap ends.
        vm.prank(admin);
        attestation.retireVerifier(address(verifier));

        assertTrue(
            attestation.isBlessedFor(executor, block.chainid, address(vault), bundle),
            "a key the new checker accepts was dropped when the old one retired"
        );
        // Which is what lets anyone cut the live executor off at the venue.
        vm.prank(alice);
        vm.expectRevert(Station.StillBlessed.selector);
        station.revokeKey("local", executor);
    }

    /// A key blessed through the old checker alone, before the new one was
    /// ready, is revived by re-sending the same proof once the new one
    /// accepts it.
    function test_a_proof_the_new_checker_accepts_can_be_re_blessed_after_the_old_one_retires() public {
        address vault = makeAddr("vault");
        bytes32 bundle = keccak256("bundle");
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, leafFor(vault, bundle));
        bytes memory seal = sealOf(journal);
        // The service keys were blessed in setUp, before the new checker was
        // ready, so through the old one: they drop at its retirement.
        vm.prank(admin);
        attestation.retireVerifier(address(verifier));
        assertFalse(attestation.isBlessedAs(builder, ImageKind.Builder));

        // The fixed checker accepts this very proof, and it is well within
        // its TTL.
        assertTrue(fixedChecker.verify(seal, journal));
        attestation.bless(seal, journal, block.chainid, vault, bundle, new bytes32[](0));
        assertTrue(
            attestation.isBlessedFor(executor, block.chainid, vault, bundle),
            "an allowed checker vouches for the proof, but the blessing stays dead"
        );
    }

    /// Service keys that attest during the overlap survive it too.
    function test_service_keys_survive_the_rotation_too() public {
        skip(1);
        blessServiceKey(BUILDER_IMAGE, builder);
        blessServiceKey(REVIEWER_IMAGE, reviewer);
        assertTrue(attestation.isBlessedAs(builder, ImageKind.Builder));
        vm.prank(admin);
        attestation.retireVerifier(address(verifier));
        assertTrue(attestation.isBlessedAs(builder, ImageKind.Builder), "the builder key was dropped");
        assertTrue(attestation.isBlessedAs(reviewer, ImageKind.Reviewer), "the reviewer key was dropped");
    }
}
