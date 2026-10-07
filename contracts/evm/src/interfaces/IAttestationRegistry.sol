// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// What a measured image is for. An image is allowlisted as one kind, and a
/// key blessed from it can only do that kind's job.
enum ImageKind {
    None,
    Executor,
    Unwinder,
    Builder,
    Reviewer,
    /// A dedicated holder of the root key. It holds no key for any vault,
    /// signs no build or review, and can do nothing on chain but record the
    /// root key's public half: `bless` refuses it.
    Holder
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

/// The root key's public half, as the registry recorded it.
///
/// The root key is made inside an attested instance and only ever passes
/// between attested instances; every key derived from it is signed by
/// `signer`. Anyone sealing a secret to a derived key checks that signature
/// against this record, and that the record is live.
struct RootKeyRecord {
    /// The address of the root's signing key; zero while none is recorded.
    address signer;
    /// When the instance that last recorded it attested.
    uint64 issuedAt;
    /// The checker that accepted that proof.
    address verifier;
    /// When it was last recorded: first set, or moved to a later
    /// attestation or a newer checker.
    uint64 setAt;
    /// The image of the instance that last recorded it.
    bytes32 imageDigest;
    /// The operator project that instance ran in.
    bytes32 projectId;
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
    event DriverProposalWithdrawn(bytes32 indexed name, address indexed implementation);
    event BridgeProposed(bytes32 indexed name, address indexed implementation, uint64 readyAt);
    event BridgeRevoked(bytes32 indexed name, address indexed implementation);
    event BridgeProposalWithdrawn(bytes32 indexed name, address indexed implementation);
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

    /// Bless `key`, a key the attested instance generated, for one vault
    /// and bundle (or, for a builder or a reviewer, for none).
    ///
    /// `journal` is what the instance attested to: the image it is, the
    /// project it runs in, its own key (which names the instance and is not
    /// blessed by this call), the root of the
    /// (vault chain, vault, bundle, chain, key) leaves it holds keys for, and
    /// when it attested. `seal` is the proof over that journal. `merkleProof`
    /// shows that this vault and this bundle, on this chain, with `key`,
    /// are among them; a builder or a reviewer proves the leaf with no vault
    /// and its own key, on this chain. One proof so blesses every key the
    /// instance holds, each for its own vault only. Anyone may send it, and
    /// sending it again changes nothing: a blessing lasts a TTL from the
    /// attestation, not from the transaction. A holder's image blesses
    /// nothing.
    function bless(
        bytes calldata seal,
        bytes calldata journal,
        uint256 vaultChain,
        address vault,
        bytes32 strategyHash,
        address key,
        bytes32[] calldata merkleProof
    ) external;

    /// The root key was recorded: set for the first time since a clear, or,
    /// for the signer already recorded, moved to a newer checker.
    event RootKeySet(
        address indexed signer, bytes32 imageDigest, bytes32 projectId, address verifier, uint64 issuedAt
    );
    /// The record was cleared, and its signer can never be recorded again.
    event RootKeyCleared(address indexed signer);

    /// Record `rootSigner` as the root key's signing address.
    ///
    /// The proof is checked as a blessing's is: an allowed checker accepts
    /// `seal` over `journal`, the image is allowed as an executor or a holder,
    /// the project is allowed, and the attestation is within a blessing's TTL.
    /// `merkleProof` shows the root leaf, which names `rootSigner` on this
    /// chain, is among the instance's leaves, so the same attestation that
    /// blesses an executor's vault keys also carries its root. Anyone may
    /// send it.
    ///
    /// While a record is in place, another signer is refused. The same
    /// signer is recorded again from an attestation no earlier than the
    /// recorded one, from any allowed executor or holder image and project,
    /// through any allowed checker; the same attestation again (same time,
    /// image and project) moves only to a newer checker, or off one no longer
    /// allowed. So the record follows the root to another image that
    /// received it by attested exchange, a checker rotation's overlap carries
    /// it past the old checker's retirement, and a re-attestation revives it
    /// after its image's successor or its checker takes over. A record has to
    /// be cleared before another root can be set, only an attestation made
    /// after the clear can set it, and a cleared signer is never recorded
    /// again.
    function setRootKey(
        bytes calldata seal,
        bytes calldata journal,
        address rootSigner,
        bytes32[] calldata merkleProof
    ) external;
    /// Admin only, at once: forget the root key, to rotate it or because it
    /// is lost, and refuse its signer for good. The admin can clear a record
    /// but never make one.
    function clearRootKey() external;
    /// The record, whether or not it is still live.
    function rootKey() external view returns (RootKeyRecord memory);
    /// Whether a root key is recorded and the image, the project and the
    /// checker of the attestation that last recorded it are all still
    /// allowed. Revoking any of those three makes the root untrusted at once.
    ///
    /// The registry knows only the instance that last recorded the root, not
    /// every image that has ever held it, so revoking another image that
    /// held it leaves the record live. When an image is revoked for a flaw
    /// that could have exposed the root, the admin clears the record as well,
    /// which retires that root for good and lets a new one be set.
    function isRootKeyLive() external view returns (bool);
    /// The purpose the root leaf carries where a key's leaf carries its
    /// bundle; `bless` refuses it as a bundle.
    function ROOT_KEY_PURPOSE() external view returns (bytes32);

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

    /// The on-chain half of a driver, by its name in `shonin.toml`. It is
    /// approved once its timelock passes, and then only for stations created
    /// afterwards: a station pins the implementations in force when it is
    /// created and keeps them for good, so nothing approved later reaches a
    /// running vault.
    function proposeDriver(bytes32 name, address implementation) external;
    /// Withdraw the proposal still waiting out its timelock; nothing in force
    /// changes.
    function withdrawDriverProposal(bytes32 name) external;
    /// At once: stations that pin `implementation` stop trading through it,
    /// and keep valuing and closing through it, so an exit never depends on
    /// a driver's approval.
    function revokeDriver(bytes32 name, address implementation) external;
    /// What a station created now pins: zero while none is approved, or once
    /// the latest approval is revoked.
    function tradingDriver(bytes32 name) external view returns (address);
    function isDriverRevoked(bytes32 name, address implementation) external view returns (bool);

    /// A bridge, by its name in `shonin.toml`, approved and pinned the same
    /// way. A station sends nothing through a revoked bridge, even home.
    function proposeBridge(bytes32 name, address implementation) external;
    function withdrawBridgeProposal(bytes32 name) external;
    function revokeBridge(bytes32 name, address implementation) external;
    /// Zero while none is approved, or once the latest approval is revoked.
    function bridgeOf(bytes32 name) external view returns (address);
    function isBridgeRevoked(bytes32 name, address implementation) external view returns (bool);

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
