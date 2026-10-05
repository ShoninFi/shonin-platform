// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../../src/AttestationRegistry.sol";
import {DevAttestationVerifier} from "../../src/DevAttestationVerifier.sol";
import {ConfidentialSpaceVerifier} from "../../src/attestation/ConfidentialSpaceVerifier.sol";
import {ImageKind, KeyInfo} from "../../src/interfaces/IAttestationRegistry.sol";
import {Groth16Fixture as F} from "./fixtures/Groth16Fixture.sol";
import {Test, console} from "forge-std/Test.sol";
import {IRiscZeroVerifier} from "risc0/IRiscZeroVerifier.sol";
import {ControlID} from "risc0/groth16/ControlID.sol";
import {RiscZeroGroth16Verifier} from "risc0/groth16/RiscZeroGroth16Verifier.sol";

/// The Confidential Space checker against a real RISC Zero Groth16 proof of
/// the attestation guest (over a test token signed by a test key; see the
/// fixture).
contract ConfidentialSpaceVerifierTest is Test {
    IRiscZeroVerifier riscZero;
    ConfidentialSpaceVerifier checker;
    address admin = makeAddr("admin");

    function setUp() public {
        riscZero = new RiscZeroGroth16Verifier(ControlID.CONTROL_ROOT, ControlID.BN254_CONTROL_ID);
        checker = new ConfidentialSpaceVerifier(riscZero, F.IMAGE_ID, keys(F.JWK_HASH));
        vm.warp(F.ISSUED_AT + 600);
    }

    function keys(bytes32 key) internal pure returns (bytes32[] memory list) {
        list = new bytes32[](1);
        list[0] = key;
    }

    function flipped(bytes memory data, uint256 at) internal pure returns (bytes memory copy) {
        copy = bytes.concat(data);
        copy[at] = bytes1(uint8(copy[at]) ^ 0x01);
    }

    function test_a_real_seal_of_the_attestation_guest_is_accepted() public view {
        uint256 before = gasleft();
        bool accepted = checker.verify(F.SEAL, F.JOURNAL);
        uint256 used = before - gasleft();
        console.log("checker.verify gas", used);
        assertTrue(accepted);
        assertLt(used, 1_000_000, "within the registry's checker budget");
    }

    function test_every_field_of_the_journal_and_the_seal_s_tail_is_bound_by_the_proof() public view {
        // One byte in each journal field: image digest, project, signer,
        // vaults root, issued at.
        uint256[5] memory journalBytes = [uint256(0), 63, 95, 96, 159];
        for (uint256 i = 0; i < journalBytes.length; i++) {
            assertFalse(checker.verify(F.SEAL, flipped(F.JOURNAL, journalBytes[i])), "a journal byte");
        }
        // The Solana key after the Groth16 seal, and the proof itself.
        assertFalse(checker.verify(flipped(F.SEAL, 260), F.JOURNAL), "the Solana key");
        assertFalse(checker.verify(flipped(F.SEAL, 100), F.JOURNAL), "the proof");
    }

    function test_a_malformed_proof_costs_at_most_the_risc_zero_gas_cap() public view {
        uint256 before = gasleft();
        assertFalse(checker.verify(flipped(F.SEAL, 100), F.JOURNAL));
        assertLt(before - gasleft(), checker.RISC_ZERO_GAS() + 50_000);
    }

    function test_a_token_signed_by_a_key_the_checker_does_not_trust_is_refused() public {
        ConfidentialSpaceVerifier other =
            new ConfidentialSpaceVerifier(riscZero, F.IMAGE_ID, keys(keccak256("another key")));
        assertFalse(other.verify(F.SEAL, F.JOURNAL));
    }

    function test_naming_another_trusted_key_in_the_tail_does_not_pass_the_proof() public {
        bytes32 second = keccak256("a second trusted key");
        bytes32[] memory both = new bytes32[](2);
        both[0] = F.JWK_HASH;
        both[1] = second;
        ConfidentialSpaceVerifier two = new ConfidentialSpaceVerifier(riscZero, F.IMAGE_ID, both);
        assertTrue(two.verify(F.SEAL, F.JOURNAL));
        bytes memory seal = bytes.concat(F.SEAL);
        for (uint256 i = 0; i < 32; i++) {
            seal[292 + i] = second[i];
        }
        assertFalse(two.verify(seal, F.JOURNAL));
    }

    function test_a_seal_of_another_guest_is_refused() public {
        ConfidentialSpaceVerifier other =
            new ConfidentialSpaceVerifier(riscZero, keccak256("another guest"), keys(F.JWK_HASH));
        assertFalse(other.verify(F.SEAL, F.JOURNAL));
    }

    function test_malformed_input_is_answered_no_without_reverting() public view {
        assertFalse(checker.verify("", ""));
        assertFalse(checker.verify(F.SEAL, ""));
        assertFalse(checker.verify(hex"", F.JOURNAL));
        assertFalse(checker.verify(bytes.concat(F.SEAL, hex"00"), F.JOURNAL));
        assertFalse(checker.verify(F.SEAL, bytes.concat(F.JOURNAL, hex"00")));
        assertFalse(checker.verify(new bytes(324), F.JOURNAL));
        // Bytes `abi.decode` would ignore must be zero: above the signer's
        // address and above the 64-bit time.
        assertFalse(checker.verify(F.SEAL, flipped(F.JOURNAL, 64)));
        assertFalse(checker.verify(F.SEAL, flipped(F.JOURNAL, 128)));
    }

    function test_the_constructor_refuses_a_missing_verifier_image_or_key_set() public {
        vm.expectRevert(ConfidentialSpaceVerifier.NoVerifier.selector);
        new ConfidentialSpaceVerifier(IRiscZeroVerifier(address(0)), F.IMAGE_ID, keys(F.JWK_HASH));
        vm.expectRevert(ConfidentialSpaceVerifier.NoImage.selector);
        new ConfidentialSpaceVerifier(riscZero, bytes32(0), keys(F.JWK_HASH));
        vm.expectRevert(ConfidentialSpaceVerifier.NoKeys.selector);
        new ConfidentialSpaceVerifier(riscZero, F.IMAGE_ID, new bytes32[](0));
    }

    function test_the_trust_root_names_confidential_space_on_tdx() public view {
        assertEq(checker.trustRoot(), bytes32("gcp-cs-tdx"));
        assertEq(checker.trustedKeys().length, 1);
        assertTrue(checker.isTrustedKey(F.JWK_HASH));
    }

    // Through the registry ---------------------------------------------------

    function registry(uint64 timelock) internal returns (AttestationRegistry attestation) {
        DevAttestationVerifier dev = new DevAttestationVerifier(makeAddr("attestor"));
        vm.startPrank(admin);
        attestation = new AttestationRegistry(admin, dev, timelock, 0, 72 hours);
        attestation.proposeVerifier(address(checker));
        attestation.proposeImage(F.IMAGE_DIGEST, ImageKind.Executor);
        attestation.proposeProject(F.PROJECT_ID);
        vm.stopPrank();
    }

    function bless(AttestationRegistry attestation) internal {
        attestation.bless(F.SEAL, F.JOURNAL, F.VAULT_CHAIN, F.VAULT, F.STRATEGY_HASH, new bytes32[](0));
    }

    function test_the_registry_blesses_an_executor_through_the_checker_within_budget() public {
        AttestationRegistry attestation = registry(0);
        uint256 before = gasleft();
        bless(attestation);
        uint256 used = before - gasleft();
        console.log("registry.bless gas, a first blessing", used);
        assertLt(used, 1_000_000);

        assertTrue(attestation.isBlessedFor(F.SIGNER, F.VAULT_CHAIN, F.VAULT, F.STRATEGY_HASH));
        KeyInfo memory info = attestation.keyInfo(F.SIGNER);
        assertEq(info.verifier, address(checker));
        assertEq(info.trustRoot, bytes32("gcp-cs-tdx"));
        assertEq(info.imageDigest, F.IMAGE_DIGEST);
        assertEq(info.projectId, F.PROJECT_ID);
        assertEq(info.blessedUntil, F.ISSUED_AT + 72 hours);

        // Renewing with the same proof changes nothing and costs no more.
        before = gasleft();
        bless(attestation);
        console.log("registry.bless gas, the same proof again", before - gasleft());
    }

    function test_the_checker_counts_only_once_its_timelock_has_passed_beside_the_dev_checker() public {
        AttestationRegistry attestation = registry(2 days);
        vm.warp(block.timestamp + 2 days);
        // The image and project wait as long as the checker, so they are
        // ready only now; one second earlier the checker was not.
        vm.warp(block.timestamp - 1);
        vm.expectRevert(AttestationRegistry.ProofRejected.selector);
        bless(attestation);
        vm.warp(block.timestamp + 1);
        bless(attestation);
        assertTrue(attestation.isBlessedFor(F.SIGNER, F.VAULT_CHAIN, F.VAULT, F.STRATEGY_HASH));
        // The development checker is still allowed beside it.
        assertEq(attestation.verifiers().length, 2);
    }

    function test_retiring_the_checker_retires_the_keys_it_blessed() public {
        AttestationRegistry attestation = registry(0);
        bless(attestation);
        vm.prank(admin);
        attestation.retireVerifier(address(checker));
        assertFalse(attestation.isBlessedFor(F.SIGNER, F.VAULT_CHAIN, F.VAULT, F.STRATEGY_HASH));
    }

    function test_a_proof_older_than_the_blessing_ttl_blesses_nothing() public {
        AttestationRegistry attestation = registry(0);
        vm.warp(F.ISSUED_AT + 72 hours);
        vm.expectRevert(AttestationRegistry.Stale.selector);
        bless(attestation);
    }
}
