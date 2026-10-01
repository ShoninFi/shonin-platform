// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationRegistry, ImageKind} from "./interfaces/IAttestationRegistry.sol";
import {IPublishing} from "./interfaces/IPublishing.sol";
import {ChainNeed, IStrategyRegistry, Strategy} from "./interfaces/IStrategyRegistry.sol";
import {ECDSA} from "openzeppelin/utils/cryptography/ECDSA.sol";
import {MessageHashUtils} from "openzeppelin/utils/cryptography/MessageHashUtils.sol";

/// Every strategy there is.
///
/// Anyone may send a registration; only a blessed builder's signature over
/// what it built, and a blessed reviewer's go-ahead for the source it read,
/// make it stick. A vault asks this contract what its strategy's bundle is
/// and gets an answer nobody could have forged.
contract StrategyRegistry is IStrategyRegistry {
    IAttestationRegistry public immutable attestation;
    IPublishing public immutable publishing;

    mapping(uint256 => Strategy) private _strategy;

    error NotTheBuilder();
    error NotCleared();
    error AlreadyRegistered();
    error NotSubmittedByTheAuthor();
    error NotSorted();
    error NothingBuilt();
    error BadSignature();

    constructor(IAttestationRegistry attestation_, IPublishing publishing_) {
        attestation = attestation_;
        publishing = publishing_;
    }

    function register(
        uint256 strategyId,
        address author,
        bytes32 sourceHash,
        bytes32 bundleHash,
        bytes32 manifestHash,
        ChainNeed[] calldata chains,
        bytes32[] calldata bridges,
        bytes32[] calldata restricted,
        bytes calldata builderSignature,
        bytes calldata reviewerClearance
    ) external {
        if (_strategy[strategyId].registeredAt != 0) revert AlreadyRegistered();
        // Zero means "nothing" everywhere else: not registered, or refused.
        if (sourceHash == 0 || bundleHash == 0) revert NothingBuilt();
        // The id belongs to whoever paid for it, and to nobody else.
        if (author == address(0) || publishing.submissionOf(strategyId).author != author) {
            revert NotSubmittedByTheAuthor();
        }
        for (uint256 i = 1; i < restricted.length; i++) {
            if (restricted[i] <= restricted[i - 1]) revert NotSorted();
        }

        bytes32 digest = registrationDigest(
            strategyId, author, sourceHash, bundleHash, manifestHash, chains, bridges, restricted
        );
        if (!attestation.isBlessedAs(_signer(digest, builderSignature), ImageKind.Builder)) {
            revert NotTheBuilder();
        }
        // The review comes before the build: the reviewer has to have read
        // this source and let it through before anything compiled it.
        if (
            !attestation.isBlessedAs(
                _signer(clearanceDigest(strategyId, sourceHash), reviewerClearance), ImageKind.Reviewer
            )
        ) revert NotCleared();

        Strategy storage entry = _strategy[strategyId];
        entry.author = author;
        entry.sourceHash = sourceHash;
        entry.bundleHash = bundleHash;
        entry.manifestHash = manifestHash;
        entry.registeredAt = uint64(block.timestamp);
        for (uint256 i = 0; i < chains.length; i++) {
            ChainNeed storage need = entry.chains.push();
            need.chain = chains[i].chain;
            for (uint256 j = 0; j < chains[i].drivers.length; j++) {
                need.drivers.push(chains[i].drivers[j]);
            }
        }
        for (uint256 i = 0; i < bridges.length; i++) {
            entry.bridges.push(bridges[i]);
        }
        for (uint256 i = 0; i < restricted.length; i++) {
            entry.restricted.push(restricted[i]);
        }
        emit Registered(strategyId, author, bundleHash, sourceHash, manifestHash);
    }

    function _signer(bytes32 digest, bytes calldata signature) private pure returns (address signer) {
        ECDSA.RecoverError err;
        (signer, err,) = ECDSA.tryRecover(MessageHashUtils.toEthSignedMessageHash(digest), signature);
        if (err != ECDSA.RecoverError.NoError) revert BadSignature();
    }

    function isRegistered(uint256 strategyId) external view returns (bool) {
        return _strategy[strategyId].registeredAt != 0;
    }

    function strategyOf(uint256 strategyId) external view returns (Strategy memory) {
        return _strategy[strategyId];
    }

    function bundleHashOf(uint256 strategyId) external view returns (bytes32) {
        return _strategy[strategyId].bundleHash;
    }

    function authorOf(uint256 strategyId) external view returns (address) {
        return _strategy[strategyId].author;
    }

    function registrationDigest(
        uint256 strategyId,
        address author,
        bytes32 sourceHash,
        bytes32 bundleHash,
        bytes32 manifestHash,
        ChainNeed[] calldata chains,
        bytes32[] calldata bridges,
        bytes32[] calldata restricted
    ) public view returns (bytes32) {
        return keccak256(
            abi.encode(
                block.chainid,
                address(this),
                "shonin-strategy-registration",
                strategyId,
                author,
                sourceHash,
                bundleHash,
                manifestHash,
                keccak256(abi.encode(chains)),
                keccak256(abi.encode(bridges)),
                keccak256(abi.encode(restricted))
            )
        );
    }

    function clearanceDigest(uint256 strategyId, bytes32 sourceHash) public view returns (bytes32) {
        return keccak256(
            abi.encode(block.chainid, address(this), "shonin-review-clearance", strategyId, sourceHash)
        );
    }
}
