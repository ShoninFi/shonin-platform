// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationVerifier} from "./interfaces/IAttestationRegistry.sol";
import {ECDSA} from "openzeppelin/utils/cryptography/ECDSA.sol";
import {MessageHashUtils} from "openzeppelin/utils/cryptography/MessageHashUtils.sol";

/// What a development chain trusts instead of an enclave.
///
/// A real deployment verifies the platform's attestation token and believes
/// what the hardware says. Here one key stands for the hardware, and a journal
/// it signed is accepted. Everything the registry does with the result is the
/// same either way, so the seam is the only thing that changes when a real
/// verifier takes over: the trust root says which of the two is in use, and
/// every vault page shows it.
contract DevAttestationVerifier is IAttestationVerifier {
    /// The key that stands in for the hardware.
    address public immutable attestationKey;

    constructor(address attestationKey_) {
        attestationKey = attestationKey_;
    }

    function verify(bytes calldata seal, bytes calldata journal) external view returns (bool) {
        bytes32 digest = MessageHashUtils.toEthSignedMessageHash(keccak256(journal));
        (address signer, ECDSA.RecoverError err,) = ECDSA.tryRecover(digest, seal);
        return err == ECDSA.RecoverError.NoError && signer == attestationKey;
    }

    function trustRoot() external pure returns (bytes32) {
        return "dev-local";
    }
}
