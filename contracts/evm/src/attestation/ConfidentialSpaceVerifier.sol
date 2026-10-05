// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationVerifier} from "../interfaces/IAttestationRegistry.sol";
import {IRiscZeroVerifier} from "risc0/IRiscZeroVerifier.sol";

/// Checks a zero-knowledge proof that an instance holds a genuine Google
/// Confidential Space attestation token.
///
/// The proof is a RISC Zero Groth16 seal of one guest program, pinned here by
/// its image id. The guest checks the token's RS256 signature against a
/// Google signing key, and its claims: issued by Confidential Space for
/// Shonin's audience, valid when proved, a production (non-debug) launcher on
/// Intel TDX with secure boot, a launcher version Google marks stable, no
/// operator override of the image's arguments or environment, and a nonce
/// that binds the instance's keys and the root of its vaults. It commits a
/// canonical journal that serves every kind of chain:
///
///     imageDigest (32) ‖ projectId (32) ‖ vaultsRoot (32) ‖ issuedAt (8, big-endian)
///     ‖ evmSigner (20) ‖ solanaSigner (32) ‖ keyHash (32)            = 188 bytes
///
/// The registry hands this checker its own journal, `abi.encode(imageDigest,
/// projectId, signer, vaultsRoot, uint64 issuedAt)`, and a seal of the
/// Groth16 seal (260 bytes) followed by the instance's Solana key and the
/// Google key's hash (32 bytes each). The checker rebuilds the canonical
/// journal from the two and verifies the seal against its `sha256`.
///
/// Which Google keys are trusted is fixed when the checker is deployed, as
/// the guest is: either changing is a new checker, proposed through the
/// registry's timelock beside this one, with this one still accepted in the
/// meantime. Nothing about it can be changed by anyone.
contract ConfidentialSpaceVerifier is IAttestationVerifier {
    /// RISC Zero's Groth16 verifier.
    IRiscZeroVerifier public immutable riscZero;
    /// The attestation guest's image id.
    bytes32 public immutable imageId;

    /// A 4-byte selector, then the 256-byte Groth16 proof.
    uint256 public constant GROTH16_SEAL_LENGTH = 260;
    /// The Groth16 seal, the instance's Solana key and the Google key's hash.
    uint256 public constant SEAL_LENGTH = GROTH16_SEAL_LENGTH + 64;
    /// `abi.encode` of the registry's five-word journal.
    uint256 public constant JOURNAL_LENGTH = 160;
    /// The gas RISC Zero's verifier is given: about twice what a valid seal
    /// costs. A malformed proof can make the pairing precompile burn all the
    /// gas it is handed, and this keeps that from being the registry's whole
    /// budget for a checker.
    uint256 public constant RISC_ZERO_GAS = 500_000;

    /// The Google signing keys trusted, by the guest's key hash
    /// (`sha256(len(n) ‖ n ‖ len(e) ‖ e)`).
    mapping(bytes32 => bool) public isTrustedKey;
    bytes32[] private _trustedKeys;

    error NoVerifier();
    error NoImage();
    error NoKeys();

    constructor(IRiscZeroVerifier riscZero_, bytes32 imageId_, bytes32[] memory trustedKeys_) {
        if (address(riscZero_) == address(0)) revert NoVerifier();
        if (imageId_ == 0) revert NoImage();
        if (trustedKeys_.length == 0) revert NoKeys();
        riscZero = riscZero_;
        imageId = imageId_;
        for (uint256 i = 0; i < trustedKeys_.length; i++) {
            if (!isTrustedKey[trustedKeys_[i]]) {
                isTrustedKey[trustedKeys_[i]] = true;
                _trustedKeys.push(trustedKeys_[i]);
            }
        }
    }

    function trustedKeys() external view returns (bytes32[] memory) {
        return _trustedKeys;
    }

    /// Whether `seal` proves `journal`. It answers, and never reverts,
    /// whatever it is given.
    function verify(bytes calldata seal, bytes calldata journal) external view returns (bool) {
        if (seal.length != SEAL_LENGTH || journal.length != JOURNAL_LENGTH) return false;
        // The signer and the time fill the low bytes of their words. Bytes
        // the registry's decoding would ignore must be zero, so one journal
        // has exactly one encoding.
        if (uint256(bytes32(journal[64:96])) >> 160 != 0) return false;
        if (uint256(bytes32(journal[128:160])) >> 64 != 0) return false;
        if (!isTrustedKey[bytes32(seal[GROTH16_SEAL_LENGTH + 32:SEAL_LENGTH])]) return false;

        bytes32 journalDigest = sha256(
            bytes.concat(
                journal[0:64], // image digest, project id
                journal[96:128], // vaults root
                journal[152:160], // issued at, big-endian
                journal[76:96], // the EVM signer
                seal[GROTH16_SEAL_LENGTH:SEAL_LENGTH] // the Solana signer, the key hash
            )
        );
        try riscZero.verify{gas: RISC_ZERO_GAS}(seal[0:GROTH16_SEAL_LENGTH], imageId, journalDigest) {
            return true;
        } catch {
            return false;
        }
    }

    /// Google Confidential Space on Intel TDX.
    function trustRoot() external pure returns (bytes32) {
        return "gcp-cs-tdx";
    }
}
