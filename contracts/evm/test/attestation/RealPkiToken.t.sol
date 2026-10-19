// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ConfidentialSpaceVerifier} from "../../src/attestation/ConfidentialSpaceVerifier.sol";
import {RealOidcTokenCurrentGuestFixture as O} from "./RealOidcTokenCurrentGuestFixture.sol";
import {RealPkiTokenFixture as P} from "./RealPkiTokenFixture.sol";
import {RealTokenFixture as R} from "./RealTokenFixture.sol";
import {Test, console} from "forge-std/Test.sol";
import {IRiscZeroVerifier} from "risc0/IRiscZeroVerifier.sol";
import {ControlID} from "risc0/groth16/ControlID.sol";
import {RiscZeroGroth16Verifier} from "risc0/groth16/RiscZeroGroth16Verifier.sol";

/// A checker that pins Google's Confidential Space Root CA, against a real
/// RISC Zero Groth16 proof of the attestation guest over a genuine PKI token
/// (its signing key's certificate chain in its header) from the same VM as
/// the OIDC token of `RealTokenFixture` (see the fixture). Such a checker
/// needs no change when Google rotates the keys it signs with.
contract RealPkiTokenTest is Test {
    IRiscZeroVerifier riscZero;

    function setUp() public {
        riscZero = new RiscZeroGroth16Verifier(ControlID.CONTROL_ROOT, ControlID.BN254_CONTROL_ID);
    }

    function only(bytes32 hash) internal pure returns (bytes32[] memory list) {
        list = new bytes32[](1);
        list[0] = hash;
    }

    /// Google's root and Google's signing keys as pinned: the checker a
    /// rotation deploys, so proofs of OIDC tokens from machines that still
    /// ask for them bless through it too.
    function rootAndKeys() internal pure returns (bytes32[] memory list) {
        bytes32[] memory keys = P.googleKeys();
        list = new bytes32[](keys.length + 1);
        list[0] = P.ROOT_HASH;
        for (uint256 i = 0; i < keys.length; i++) {
            list[i + 1] = keys[i];
        }
    }

    function checker(
        bytes32 imageId,
        bytes32[] memory trusted
    ) internal returns (ConfidentialSpaceVerifier) {
        return new ConfidentialSpaceVerifier(riscZero, imageId, trusted);
    }

    function flipped(bytes memory data, uint256 at) internal pure returns (bytes memory copy) {
        copy = bytes.concat(data);
        copy[at] = bytes1(uint8(copy[at]) ^ 0x01);
    }

    function test_the_seal_names_google_s_root_certificate_after_the_proof() public pure {
        bytes memory tail = bytes.concat(P.SEAL);
        bytes32 named;
        assembly {
            named := mload(add(add(tail, 32), 292))
        }
        assertEq(named, P.ROOT_HASH);
        // sha256 of the root certificate's DER, as Google publishes it.
        assertEq(P.ROOT_HASH, 0x148b293821bb0c6a317f413c8ba475814091cb22d49b9e3c94198db8e8f86c39);
    }

    function test_a_seal_over_a_pki_token_is_accepted_by_a_checker_pinning_google_s_root() public {
        ConfidentialSpaceVerifier root = checker(P.IMAGE_ID, only(P.ROOT_HASH));
        uint256 before = gasleft();
        assertTrue(root.verify(P.SEAL, P.JOURNAL));
        console.log("checker.verify gas, a real PKI token", before - gasleft());
        assertTrue(checker(P.IMAGE_ID, rootAndKeys()).verify(P.SEAL, P.JOURNAL));
    }

    function test_the_journal_names_the_same_image_project_signer_and_root_as_the_oidc_token() public pure {
        (bytes32 image, bytes32 project, address signer, bytes32 root, uint64 issuedAt) =
            abi.decode(P.JOURNAL, (bytes32, bytes32, address, bytes32, uint64));
        assertEq(image, R.IMAGE_DIGEST);
        assertEq(project, R.PROJECT_ID);
        assertEq(signer, R.SIGNER);
        assertEq(root, R.VAULTS_ROOT);
        // Issued a second after the OIDC token, by the same VM.
        assertEq(issuedAt, R.ISSUED_AT + 1);
        assertEq(issuedAt, P.ISSUED_AT);
    }

    function test_a_checker_trusting_only_google_s_signing_keys_refuses_a_pki_token() public {
        assertFalse(checker(P.IMAGE_ID, P.googleKeys()).verify(P.SEAL, P.JOURNAL));
    }

    function test_a_checker_pinning_another_root_refuses_a_pki_token() public {
        assertFalse(checker(P.IMAGE_ID, only(keccak256("another root"))).verify(P.SEAL, P.JOURNAL));
    }

    function test_naming_a_trusted_signing_key_instead_of_the_root_does_not_pass_the_proof() public {
        ConfidentialSpaceVerifier both = checker(P.IMAGE_ID, rootAndKeys());
        bytes32 key = P.googleKeys()[0];
        bytes memory seal = bytes.concat(P.SEAL);
        for (uint256 i = 0; i < 32; i++) {
            seal[292 + i] = key[i];
        }
        assertTrue(both.isTrustedKey(key));
        assertFalse(both.verify(seal, P.JOURNAL));
    }

    function test_a_tampered_journal_or_proof_is_refused() public {
        ConfidentialSpaceVerifier root = checker(P.IMAGE_ID, only(P.ROOT_HASH));
        uint256[5] memory at = [uint256(0), 63, 95, 96, 159];
        for (uint256 i = 0; i < at.length; i++) {
            assertFalse(root.verify(P.SEAL, flipped(P.JOURNAL, at[i])), "a journal byte");
        }
        assertFalse(root.verify(flipped(P.SEAL, 100), P.JOURNAL), "the proof");
        assertFalse(root.verify(flipped(P.SEAL, 260), P.JOURNAL), "the Solana key");
        assertFalse(root.verify(flipped(P.SEAL, 300), P.JOURNAL), "the root's hash");
    }

    function test_a_checker_of_the_earlier_guest_refuses_the_new_guest_s_seal() public {
        assertTrue(P.IMAGE_ID != R.IMAGE_ID, "the guest changed");
        assertFalse(checker(R.IMAGE_ID, rootAndKeys()).verify(P.SEAL, P.JOURNAL));
    }

    function test_the_earlier_guest_s_seals_count_only_through_the_earlier_checker() public {
        // The overlap: the earlier checker still accepts what the earlier
        // guest proved; the new checker, trusting the same keys, does not.
        assertTrue(checker(R.IMAGE_ID, R.googleKeys()).verify(R.SEAL, R.JOURNAL));
        assertFalse(checker(P.IMAGE_ID, rootAndKeys()).verify(R.SEAL, R.JOURNAL));
    }

    function test_the_new_guest_s_proof_of_an_oidc_token_passes_through_google_s_keys_not_the_root() public {
        // The same guest proves the VM's OIDC token, naming the Google key
        // that signed it: a checker trusting the root and the keys takes it
        // (machines not yet asking for PKI tokens), one trusting the root
        // alone does not.
        assertEq(O.IMAGE_ID, P.IMAGE_ID);
        assertEq(O.JWK_HASH, R.JWK_HASH);
        assertTrue(checker(P.IMAGE_ID, rootAndKeys()).verify(O.SEAL, O.JOURNAL));
        assertFalse(checker(P.IMAGE_ID, only(P.ROOT_HASH)).verify(O.SEAL, O.JOURNAL));
        // The OIDC token's journal is the earlier guest's, word for word.
        assertEq(O.JOURNAL, R.JOURNAL);
    }
}
