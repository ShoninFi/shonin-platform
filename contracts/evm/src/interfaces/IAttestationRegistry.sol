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

/// What the registry knows about one blessed key: its image and project, and
/// its latest blessing. A key generated inside an instance that serves
/// several vaults is blessed for each of them separately; `isBlessedFor`
/// answers for one.
struct KeyInfo {
    /// The image the key was generated inside.
    bytes32 imageDigest;
    /// The operator project the instance runs in.
    bytes32 projectId;
    ImageKind kind;
    /// The bundle the key may run, for an executor or an unwinder key; zero
    /// for a builder or a reviewer, which serve every strategy.
    bytes32 strategyHash;
    /// The chain the vault it serves lives on, by chain id; zero for a
    /// builder or a reviewer. A vault's stations on other chains ask about
    /// keys by the vault's chain and address together.
    uint256 vaultChain;
    /// The one vault it may act for; zero for a builder or a reviewer.
    address vault;
    /// What the proof was checked against.
    bytes32 trustRoot;
    /// The checker that accepted it. Retiring the checker retires the key.
    address verifier;
    /// When its last blessing expires: a TTL after the attestation it came
    /// from was issued. Attest again to renew.
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
/// Adding an image, an operator project, an attestation checker, a driver or
/// a bridge is timelocked and public, so an outsider can rebuild it, object,
/// and leave before it counts. Removing one is immediate, because the reason
/// to remove one is always urgent.
interface IAttestationRegistry {
    event ImageProposed(bytes32 indexed imageDigest, uint8 kind, uint64 readyAt);
    event ImageRevoked(bytes32 indexed imageDigest);
    event ProjectProposed(bytes32 indexed projectId, uint64 readyAt);
    event ProjectRevoked(bytes32 indexed projectId);
    event VerifierProposed(address indexed verifier, uint64 readyAt);
    event VerifierRetired(address indexed verifier);
    event DriverProposed(bytes32 indexed name, address indexed implementation, uint64 readyAt);
    event DriverRevoked(bytes32 indexed name, address indexed implementation);
    event BridgeProposed(bytes32 indexed name, address indexed implementation, uint64 readyAt);
    event BridgeRevoked(bytes32 indexed name, address indexed implementation);
    /// The timelock in force changes at `effectiveAt`: at once when raised,
    /// after the current timelock when lowered.
    event TimelockSet(uint64 timelock, uint64 effectiveAt);
    event Blessed(
        address indexed signer,
        uint256 vaultChain,
        address indexed vault,
        bytes32 strategyHash,
        bytes32 imageDigest,
        uint64 blessedUntil
    );

    /// Bless the key the attested instance generated.
    ///
    /// `journal` is what the instance attested to: the image it is, the
    /// project it runs in, the key it generated, the root of the
    /// (vault chain, vault, bundle, chain) leaves it holds keys for, and when
    /// it attested. `seal` is the proof over that journal. `merkleProof` shows
    /// that this vault and this bundle, on this chain, are among them; a
    /// builder or a reviewer proves the leaf with no vault, on this chain.
    /// Anyone may send it, and sending it again changes nothing: a blessing
    /// lasts a TTL from the attestation, not from the transaction.
    function bless(
        bytes calldata seal,
        bytes calldata journal,
        uint256 vaultChain,
        address vault,
        bytes32 strategyHash,
        bytes32[] calldata merkleProof
    ) external;

    function keyInfo(address signer) external view returns (KeyInfo memory);
    /// When the key's blessing for this vault and bundle expires; zero if it
    /// has none.
    function blessedUntil(
        address signer,
        uint256 vaultChain,
        address vault,
        bytes32 strategyHash
    ) external view returns (uint64);
    function isBlessedFor(
        address signer,
        uint256 vaultChain,
        address vault,
        bytes32 strategyHash
    ) external view returns (bool);
    function isBlessedAs(address signer, ImageKind kind) external view returns (bool);
    function isImageAllowed(bytes32 imageDigest) external view returns (bool);
    function imageOf(bytes32 imageDigest) external view returns (Image memory);
    function isProjectAllowed(bytes32 projectId) external view returns (bool);
    function isVerifierAllowed(address verifier) external view returns (bool);
    function verifiers() external view returns (address[] memory);

    /// Admin only, and timelocked.
    function proposeImage(bytes32 imageDigest, ImageKind kind) external;
    /// Admin only, at once.
    function revokeImage(bytes32 imageDigest) external;
    function proposeProject(bytes32 projectId) external;
    function revokeProject(bytes32 projectId) external;
    /// A new attestation checker: ready after the timelock, while the ones
    /// already allowed keep working.
    function proposeVerifier(address verifier) external;
    /// At once.
    function retireVerifier(address verifier) external;

    /// The on-chain half of a driver, by its name in `shonin.toml`. A newer
    /// implementation replaces the current one when its timelock passes.
    function proposeDriver(bytes32 name, address implementation) external;
    /// Stops trading through the current implementation at once. Valuing and
    /// unwinding through it keep working.
    function revokeDriver(bytes32 name) external;
    /// The implementation to trade through: zero while none is ready, or
    /// after a revocation.
    function tradingDriver(bytes32 name) external view returns (address);
    /// The implementation to value and unwind through: the latest that became
    /// ready, even if revoked, so an exit never depends on a driver's approval.
    function unwindDriver(bytes32 name) external view returns (address);

    /// A bridge, by its name in `shonin.toml`, approved the same way.
    function proposeBridge(bytes32 name, address implementation) external;
    function revokeBridge(bytes32 name) external;
    /// Zero while none is ready, or after a revocation.
    function bridgeOf(bytes32 name) external view returns (address);

    /// Admin only. Raising applies at once; lowering applies after the
    /// timelock in force when it was asked for.
    function setTimelock(uint64 newTimelock) external;
    /// The timelock in force now.
    function timelock() external view returns (uint64);
    /// A lowering waiting to apply, and when it does; zeros if none.
    function pendingTimelock() external view returns (uint64 value, uint64 effectiveAt);
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
