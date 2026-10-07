// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ConfidentialSpaceVerifier} from "../../src/attestation/ConfidentialSpaceVerifier.sol";
import {RealTokenFixture as R} from "./RealTokenFixture.sol";
import {Test, console} from "forge-std/Test.sol";
import {IRiscZeroVerifier} from "risc0/IRiscZeroVerifier.sol";
import {ControlID} from "risc0/groth16/ControlID.sol";
import {RiscZeroGroth16Verifier} from "risc0/groth16/RiscZeroGroth16Verifier.sol";

/// The Confidential Space checker against a real RISC Zero Groth16 proof of
/// the attestation guest over a genuine token a Confidential Space VM was
/// issued, signed by Google (see the fixture).
contract RealTokenTest is Test {
    IRiscZeroVerifier riscZero;
    ConfidentialSpaceVerifier checker;

    function setUp() public {
        riscZero = new RiscZeroGroth16Verifier(ControlID.CONTROL_ROOT, ControlID.BN254_CONTROL_ID);
        checker = new ConfidentialSpaceVerifier(riscZero, R.IMAGE_ID, R.googleKeys());
    }

    function only(bytes32 key) internal pure returns (bytes32[] memory list) {
        list = new bytes32[](1);
        list[0] = key;
    }

    function flipped(bytes memory data, uint256 at) internal pure returns (bytes memory copy) {
        copy = bytes.concat(data);
        copy[at] = bytes1(uint8(copy[at]) ^ 0x01);
    }

    function test_a_seal_over_a_google_signed_token_is_accepted_by_a_checker_trusting_google_s_keys()
        public
        view
    {
        assertTrue(checker.isTrustedKey(R.JWK_HASH), "the signing key is among Google's pinned keys");
        uint256 before = gasleft();
        assertTrue(checker.verify(R.SEAL, R.JOURNAL));
        console.log("checker.verify gas, a real token", before - gasleft());
    }

    function test_the_journal_names_the_real_image_project_signer_root_and_time() public pure {
        (bytes32 image, bytes32 project, address signer, bytes32 root, uint64 issuedAt) =
            abi.decode(R.JOURNAL, (bytes32, bytes32, address, bytes32, uint64));
        assertEq(image, R.IMAGE_DIGEST);
        assertEq(project, keccak256("shonin-testnet"));
        assertEq(project, R.PROJECT_ID);
        assertEq(signer, R.SIGNER);
        assertEq(root, R.VAULTS_ROOT);
        assertEq(issuedAt, R.ISSUED_AT);
        // The root is the one leaf the token's binding names, as the
        // registry built it when the token was issued (four words).
        assertEq(
            root,
            keccak256(
                bytes.concat(keccak256(abi.encode(R.VAULT_CHAIN, R.VAULT, R.STRATEGY_HASH, R.VAULT_CHAIN)))
            )
        );
    }

    function test_a_tampered_journal_is_refused() public view {
        // One byte in each field: image digest, project, signer, vaults
        // root, issued at.
        uint256[5] memory at = [uint256(0), 63, 95, 96, 159];
        for (uint256 i = 0; i < at.length; i++) {
            assertFalse(checker.verify(R.SEAL, flipped(R.JOURNAL, at[i])), "a journal byte");
        }
        assertFalse(checker.verify(flipped(R.SEAL, 100), R.JOURNAL), "the proof");
        assertFalse(checker.verify(flipped(R.SEAL, 300), R.JOURNAL), "the key hash after the seal");
    }

    function test_a_checker_that_does_not_trust_the_signing_key_refuses_it() public {
        bytes32[] memory google = R.googleKeys();
        bytes32[] memory others = new bytes32[](google.length - 1);
        uint256 n;
        for (uint256 i = 0; i < google.length; i++) {
            if (google[i] != R.JWK_HASH) others[n++] = google[i];
        }
        assertEq(n, others.length, "the signing key is pinned exactly once");
        ConfidentialSpaceVerifier other = new ConfidentialSpaceVerifier(riscZero, R.IMAGE_ID, others);
        assertFalse(other.verify(R.SEAL, R.JOURNAL));

        ConfidentialSpaceVerifier test =
            new ConfidentialSpaceVerifier(riscZero, R.IMAGE_ID, only(keccak256("a test key")));
        assertFalse(test.verify(R.SEAL, R.JOURNAL));
    }

    function test_a_checker_pinning_another_guest_refuses_the_seal() public {
        ConfidentialSpaceVerifier other =
            new ConfidentialSpaceVerifier(riscZero, keccak256("another guest"), R.googleKeys());
        assertFalse(other.verify(R.SEAL, R.JOURNAL));
    }
}
