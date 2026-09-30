// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {
    IAttestationRegistry,
    IAttestationVerifier,
    Image,
    ImageKind,
    KeyInfo
} from "./interfaces/IAttestationRegistry.sol";
import {MerkleProof} from "openzeppelin/utils/cryptography/MerkleProof.sol";

/// Which code is allowed to hold a key, and which keys it currently holds.
///
/// An instance proves what it is, and gets a key blessed for exactly the work
/// that image does: an executor key for one vault and one bundle, a builder or
/// a reviewer key for no vault at all. Everything downstream -- a vault
/// approving a trading key, a registry accepting a build, a review being
/// believed -- asks this contract and nothing else.
///
/// Adding an image is timelocked and public, so an outsider can rebuild it,
/// disagree, and leave before it runs. Removing one is immediate, because the
/// reason to remove one is always urgent.
contract AttestationRegistry is IAttestationRegistry {
    /// What an instance attests to. The proof is over these bytes.
    struct Journal {
        bytes32 imageDigest;
        bytes32 projectId;
        /// The key generated inside the instance.
        address signer;
        /// The root of the (vault, bundle) pairs this instance holds keys for.
        /// Zero for an image that serves no particular vault.
        bytes32 vaultsRoot;
    }

    address public immutable admin;
    IAttestationVerifier public immutable verifier;
    uint64 public immutable timelock;
    uint64 public immutable blessingTtl;

    mapping(bytes32 => Image) private _image;
    mapping(bytes32 => uint64) private _projectReadyAt;
    mapping(bytes32 => bool) private _projectRevoked;
    mapping(address => KeyInfo) private _key;

    error NotAdmin();
    error AlreadyProposed();
    error ProofRejected();
    error ImageNotAllowed();
    error ProjectNotAllowed();
    error WrongKindForVault();
    error NotAmongTheVaults();

    constructor(address admin_, IAttestationVerifier verifier_, uint64 timelock_, uint64 blessingTtl_) {
        admin = admin_;
        verifier = verifier_;
        timelock = timelock_;
        blessingTtl = blessingTtl_;
    }

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    // The allowlist ---------------------------------------------------------

    function proposeImage(bytes32 imageDigest, ImageKind kind) external onlyAdmin {
        if (_image[imageDigest].allowedAt != 0) revert AlreadyProposed();
        uint64 readyAt = uint64(block.timestamp) + timelock;
        _image[imageDigest] = Image({kind: kind, allowedAt: readyAt, revoked: false});
        emit ImageProposed(imageDigest, uint8(kind), readyAt);
    }

    function revokeImage(bytes32 imageDigest) external onlyAdmin {
        _image[imageDigest].revoked = true;
        emit ImageRevoked(imageDigest);
    }

    function proposeProject(bytes32 projectId) external onlyAdmin {
        if (_projectReadyAt[projectId] != 0) revert AlreadyProposed();
        uint64 readyAt = uint64(block.timestamp) + timelock;
        _projectReadyAt[projectId] = readyAt;
        emit ProjectProposed(projectId, readyAt);
    }

    function revokeProject(bytes32 projectId) external onlyAdmin {
        _projectRevoked[projectId] = true;
        emit ProjectRevoked(projectId);
    }

    function isImageAllowed(bytes32 imageDigest) public view returns (bool) {
        Image storage image = _image[imageDigest];
        return image.allowedAt != 0 && !image.revoked && block.timestamp >= image.allowedAt;
    }

    function imageOf(bytes32 imageDigest) external view returns (Image memory) {
        return _image[imageDigest];
    }

    function isProjectAllowed(bytes32 projectId) public view returns (bool) {
        uint64 readyAt = _projectReadyAt[projectId];
        return readyAt != 0 && !_projectRevoked[projectId] && block.timestamp >= readyAt;
    }

    // Blessing --------------------------------------------------------------

    function bless(
        bytes calldata seal,
        bytes calldata journalBytes,
        address vault,
        bytes32 strategyHash,
        bytes32[] calldata merkleProof
    ) external {
        if (!verifier.verify(seal, journalBytes)) revert ProofRejected();
        Journal memory journal = abi.decode(journalBytes, (Journal));

        if (!isImageAllowed(journal.imageDigest)) revert ImageNotAllowed();
        if (!isProjectAllowed(journal.projectId)) revert ProjectNotAllowed();
        ImageKind kind = _image[journal.imageDigest].kind;

        bool servesOneVault = kind == ImageKind.Executor || kind == ImageKind.Unwinder;
        if (servesOneVault) {
            // The key may act for this vault and this bundle only, and the
            // instance had to have said so before it saw this transaction.
            bytes32 leaf = keccak256(bytes.concat(keccak256(abi.encode(vault, strategyHash))));
            if (!MerkleProof.verify(merkleProof, journal.vaultsRoot, leaf)) revert NotAmongTheVaults();
        } else {
            if (vault != address(0) || strategyHash != bytes32(0)) revert WrongKindForVault();
        }

        uint64 blessedUntil = uint64(block.timestamp) + blessingTtl;
        _key[journal.signer] = KeyInfo({
            imageDigest: journal.imageDigest,
            kind: kind,
            strategyHash: servesOneVault ? strategyHash : bytes32(0),
            vault: servesOneVault ? vault : address(0),
            trustRoot: verifier.trustRoot(),
            blessedUntil: blessedUntil
        });
        emit Blessed(journal.signer, vault, strategyHash, journal.imageDigest, blessedUntil);
    }

    function keyInfo(address signer) external view returns (KeyInfo memory) {
        return _key[signer];
    }

    function isBlessedFor(address signer, address vault, bytes32 strategyHash) external view returns (bool) {
        KeyInfo storage key = _key[signer];
        return key.blessedUntil >= block.timestamp && key.vault == vault && key.strategyHash == strategyHash
            && isImageAllowed(key.imageDigest);
    }

    function isBlessedAs(address signer, ImageKind kind) external view returns (bool) {
        KeyInfo storage key = _key[signer];
        return key.blessedUntil >= block.timestamp && key.kind == kind && isImageAllowed(key.imageDigest);
    }
}
