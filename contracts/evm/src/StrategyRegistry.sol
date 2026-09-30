// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationRegistry, ImageKind} from "./interfaces/IAttestationRegistry.sol";
import {IStrategyRegistry, StrategyVersion, VersionStatus} from "./interfaces/IStrategyRegistry.sol";
import {ECDSA} from "openzeppelin/utils/cryptography/ECDSA.sol";
import {MessageHashUtils} from "openzeppelin/utils/cryptography/MessageHashUtils.sol";

/// Every strategy version there is.
///
/// Anyone may send a registration; only a signature from a key blessed as a
/// builder makes it stick. So the bundles on the registry are exactly the ones
/// an allowlisted build service compiled from source it read, and a vault can
/// ask this contract what its strategy's bundle hash is and get an answer
/// nobody could have forged.
contract StrategyRegistry is IStrategyRegistry {
    IAttestationRegistry public immutable attestation;

    mapping(bytes32 => mapping(uint32 => StrategyVersion)) private _version;

    error NotTheBuilder();
    error AlreadyRegistered();
    error NotRegistered();
    error NotTheAuthor();

    constructor(IAttestationRegistry attestation_) {
        attestation = attestation_;
    }

    function register(
        bytes32 strategyId,
        uint32 version,
        address author,
        bytes32 sourceHash,
        bytes32 bundleHash,
        bytes32 manifestHash,
        bytes32 supersedesId,
        uint32 supersedesVersion,
        bytes calldata builderSignature
    ) external {
        if (_version[strategyId][version].status != VersionStatus.None) revert AlreadyRegistered();

        bytes32 digest = MessageHashUtils.toEthSignedMessageHash(
            registrationDigest(strategyId, version, author, sourceHash, bundleHash, manifestHash)
        );
        (address signer, ECDSA.RecoverError err,) = ECDSA.tryRecover(digest, builderSignature);
        if (err != ECDSA.RecoverError.NoError) revert NotTheBuilder();
        if (!attestation.isBlessedAs(signer, ImageKind.Builder)) revert NotTheBuilder();

        _version[strategyId][version] = StrategyVersion({
            author: author,
            sourceHash: sourceHash,
            bundleHash: bundleHash,
            manifestHash: manifestHash,
            supersedesId: supersedesId,
            supersedesVersion: supersedesVersion,
            registeredAt: uint64(block.timestamp),
            status: VersionStatus.Registered
        });
        emit Registered(strategyId, version, author, bundleHash, sourceHash, manifestHash);
    }

    function retire(bytes32 strategyId, uint32 version) external {
        StrategyVersion storage entry = _version[strategyId][version];
        if (entry.status != VersionStatus.Registered) revert NotRegistered();
        if (msg.sender != entry.author) revert NotTheAuthor();
        entry.status = VersionStatus.Retired;
        emit Retired(strategyId, version);
    }

    function versionOf(bytes32 strategyId, uint32 version) external view returns (StrategyVersion memory) {
        return _version[strategyId][version];
    }

    function bundleHashOf(bytes32 strategyId, uint32 version) external view returns (bytes32) {
        return _version[strategyId][version].bundleHash;
    }

    function registrationDigest(
        bytes32 strategyId,
        uint32 version,
        address author,
        bytes32 sourceHash,
        bytes32 bundleHash,
        bytes32 manifestHash
    ) public view returns (bytes32) {
        return keccak256(
            abi.encode(
                block.chainid,
                address(this),
                "shonin-strategy-registration",
                strategyId,
                version,
                author,
                sourceHash,
                bundleHash,
                manifestHash
            )
        );
    }
}
