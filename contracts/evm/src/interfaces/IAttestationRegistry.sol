// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// What a measured image is for. An image is allowlisted as one kind, and a
/// key blessed from it can only do that kind's job.
enum ImageKind {
    None,
    Executor,
    Unwinder,
    Builder,
    Reviewer
}

/// What the registry knows about one blessed key.
struct KeyInfo {
    /// The image the key was generated inside.
    bytes32 imageDigest;
    ImageKind kind;
    /// The bundle the key may run, for an executor key; zero for a builder or
    /// a reviewer, which serve every strategy.
    bytes32 strategyHash;
    /// The one vault it may act for; zero for a builder or a reviewer.
    address vault;
    /// What the proof was checked against.
    bytes32 trustRoot;
    /// Expiry. Attest again to renew.
    uint64 blessedUntil;
}

/// An image on the allowlist.
struct Image {
    ImageKind kind;
    /// When the timelock ran out and it became blessable. Zero until proposed.
    uint64 allowedAt;
    bool revoked;
}

/// Which code is allowed to hold a key, and which keys it currently holds.
///
/// Adding an image is timelocked and public, so an outsider can rebuild it,
/// object, and leave before it runs. Removing one is immediate, because the
/// reason to remove one is always urgent.
interface IAttestationRegistry {
    event ImageProposed(bytes32 indexed imageDigest, uint8 kind, uint64 readyAt);
    event ImageRevoked(bytes32 indexed imageDigest);
    event ProjectProposed(bytes32 indexed projectId, uint64 readyAt);
    event ProjectRevoked(bytes32 indexed projectId);
    event Blessed(
        address indexed signer,
        address indexed vault,
        bytes32 strategyHash,
        bytes32 imageDigest,
        uint64 blessedUntil
    );

    /// Bless the key the attested instance generated.
    ///
    /// `journal` is what the instance attested to: the image it is, the
    /// project it runs in, the key it generated, and the root of the vaults it
    /// holds keys for. `seal` is the proof over that journal. `merkleProof`
    /// shows that this vault and this bundle are among them.
    function bless(
        bytes calldata seal,
        bytes calldata journal,
        address vault,
        bytes32 strategyHash,
        bytes32[] calldata merkleProof
    ) external;

    function keyInfo(address signer) external view returns (KeyInfo memory);
    function isBlessedFor(address signer, address vault, bytes32 strategyHash) external view returns (bool);
    function isBlessedAs(address signer, ImageKind kind) external view returns (bool);
    function isImageAllowed(bytes32 imageDigest) external view returns (bool);
    function imageOf(bytes32 imageDigest) external view returns (Image memory);
    function isProjectAllowed(bytes32 projectId) external view returns (bool);

    /// Admin only, and timelocked.
    function proposeImage(bytes32 imageDigest, ImageKind kind) external;
    /// Admin only, at once.
    function revokeImage(bytes32 imageDigest) external;
    function proposeProject(bytes32 projectId) external;
    function revokeProject(bytes32 projectId) external;

    function timelock() external view returns (uint64);
    function blessingTtl() external view returns (uint64);
    function admin() external view returns (address);
}

/// What checks a proof. A development chain trusts a signature; a real one
/// verifies the platform's attestation token, and nothing else changes.
interface IAttestationVerifier {
    function verify(bytes calldata seal, bytes calldata journal) external view returns (bool);
    /// Names what the proof was checked against, for the vault's page.
    function trustRoot() external view returns (bytes32);
}
