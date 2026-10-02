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
/// that image does: an executor or unwinder key for one vault and one bundle,
/// a builder or a reviewer key for no vault at all. Everything downstream -- a
/// station approving a trading key, a registry accepting a build, a review
/// being believed -- asks this contract and nothing else.
///
/// Adding anything is timelocked and public, so an outsider can rebuild it,
/// disagree, and leave before it counts. Removing anything is immediate,
/// because the reason to remove one is always urgent.
contract AttestationRegistry is IAttestationRegistry {
    /// What an instance attests to. The proof is over these bytes.
    struct Journal {
        bytes32 imageDigest;
        bytes32 projectId;
        /// The key generated inside the instance.
        address signer;
        /// The root of the (vault chain, vault, bundle, chain) leaves this
        /// instance holds keys for. A builder or a reviewer commits to the
        /// leaf with no vault, for each chain it serves.
        bytes32 vaultsRoot;
        /// When the instance attested. A blessing lasts a TTL from here.
        uint64 issuedAt;
    }

    /// One approved driver or bridge name: the implementation in force, and
    /// a newer one waiting out its timelock.
    struct Slot {
        address current;
        address next;
        uint64 nextReadyAt;
    }

    address public immutable admin;
    uint64 public immutable blessingTtl;

    uint64 private _timelock;
    uint64 private _pendingTimelock;
    uint64 private _pendingTimelockAt;

    mapping(bytes32 => Image) private _image;
    mapping(bytes32 => uint64) private _projectReadyAt;
    mapping(bytes32 => bool) private _projectRevoked;
    mapping(address => KeyInfo) private _key;
    /// Each key's blessings, by `keccak256(vaultChain, vault, strategyHash)`:
    /// until when, and through which checker.
    mapping(address => mapping(bytes32 => Blessing)) private _blessing;

    struct Blessing {
        uint64 until;
        address verifier;
    }

    /// The longest the timelock can be set to, so a slip of the admin's
    /// finger can't put everything out of reach for good.
    uint64 public constant MAX_TIMELOCK = 365 days;
    /// The shortest the timelock can ever be on this deployment, fixed when
    /// it is made: two days where depositors' money is at stake, so nothing
    /// the admin proposes can apply before they have had time to leave, and
    /// zero on a development chain.
    uint64 public immutable minTimelock;

    address[] private _verifiers;
    mapping(address => uint64) private _verifierReadyAt;
    mapping(address => bool) private _verifierRetired;

    mapping(bytes32 => Slot) private _driver;
    mapping(bytes32 => Slot) private _bridge;
    /// Per name and implementation, so a fix proposed after a revocation
    /// counts again once its own timelock passes.
    mapping(bytes32 => mapping(address => bool)) private _driverRevoked;
    mapping(bytes32 => mapping(address => bool)) private _bridgeRevoked;

    error NotAdmin();
    error AlreadyProposed();
    error ProofRejected();
    error ImageNotAllowed();
    error ProjectNotAllowed();
    error WrongKindForVault();
    error NotAmongTheVaults();
    error NoImplementation();
    error NotApproved();
    error Stale();
    error TooLong();
    error TooShort();
    error NoKind();
    error NotThisInstance();
    error NoSigner();

    constructor(
        address admin_,
        IAttestationVerifier verifier_,
        uint64 timelock_,
        uint64 minTimelock_,
        uint64 blessingTtl_
    ) {
        if (timelock_ > MAX_TIMELOCK || minTimelock_ > MAX_TIMELOCK) revert TooLong();
        if (timelock_ < minTimelock_) revert TooShort();
        minTimelock = minTimelock_;
        if (address(verifier_) == address(0)) revert NoImplementation();
        admin = admin_;
        _timelock = timelock_;
        blessingTtl = blessingTtl_;
        // The first checker is trusted from the start: nothing can have been
        // blessed before it, so there is nothing for a timelock to protect.
        _verifiers.push(address(verifier_));
        _verifierReadyAt[address(verifier_)] = uint64(block.timestamp);
        emit VerifierProposed(address(verifier_), uint64(block.timestamp));
    }

    modifier onlyAdmin() {
        if (msg.sender != admin) revert NotAdmin();
        _;
    }

    // The timelock ----------------------------------------------------------

    function timelock() public view returns (uint64) {
        if (_pendingTimelockAt != 0 && block.timestamp >= _pendingTimelockAt) return _pendingTimelock;
        return _timelock;
    }

    function pendingTimelock() external view returns (uint64 value, uint64 effectiveAt) {
        if (_pendingTimelockAt == 0 || block.timestamp >= _pendingTimelockAt) return (0, 0);
        return (_pendingTimelock, _pendingTimelockAt);
    }

    function setTimelock(uint64 newTimelock) external onlyAdmin {
        if (newTimelock > MAX_TIMELOCK) revert TooLong();
        if (newTimelock < minTimelock) revert TooShort();
        uint64 current = timelock();
        _timelock = current;
        delete _pendingTimelock;
        delete _pendingTimelockAt;
        if (newTimelock >= current) {
            // Making everyone wait longer can only protect them.
            _timelock = newTimelock;
            emit TimelockSet(newTimelock, uint64(block.timestamp));
        } else {
            // Cutting the wait has to wait out the wait it cuts, so it can
            // never be used to slip something in quietly.
            uint64 effectiveAt = uint64(block.timestamp) + current;
            _pendingTimelock = newTimelock;
            _pendingTimelockAt = effectiveAt;
            emit TimelockSet(newTimelock, effectiveAt);
        }
    }

    function _readyAt() private view returns (uint64) {
        return uint64(block.timestamp) + timelock();
    }

    // Images and projects ---------------------------------------------------

    function proposeImage(bytes32 imageDigest, ImageKind kind) external onlyAdmin {
        if (kind == ImageKind.None) revert NoKind();
        if (_image[imageDigest].allowedAt != 0) revert AlreadyProposed();
        uint64 readyAt = _readyAt();
        _image[imageDigest] = Image({kind: kind, allowedAt: readyAt, revoked: false});
        emit ImageProposed(imageDigest, uint8(kind), readyAt);
    }

    function revokeImage(bytes32 imageDigest) external onlyAdmin {
        _image[imageDigest].revoked = true;
        emit ImageRevoked(imageDigest);
    }

    function proposeProject(bytes32 projectId) external onlyAdmin {
        if (_projectReadyAt[projectId] != 0) revert AlreadyProposed();
        // A revocation of something never proposed revoked nothing.
        delete _projectRevoked[projectId];
        uint64 readyAt = _readyAt();
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

    // Attestation checkers --------------------------------------------------

    function proposeVerifier(address verifier) external onlyAdmin {
        if (verifier == address(0)) revert NoImplementation();
        if (_verifierReadyAt[verifier] != 0) revert AlreadyProposed();
        // A retirement of a checker never proposed retired nothing.
        delete _verifierRetired[verifier];
        uint64 readyAt = _readyAt();
        _verifiers.push(verifier);
        _verifierReadyAt[verifier] = readyAt;
        emit VerifierProposed(verifier, readyAt);
    }

    function retireVerifier(address verifier) external onlyAdmin {
        _verifierRetired[verifier] = true;
        emit VerifierRetired(verifier);
    }

    function isVerifierAllowed(address verifier) public view returns (bool) {
        uint64 readyAt = _verifierReadyAt[verifier];
        return readyAt != 0 && !_verifierRetired[verifier] && block.timestamp >= readyAt;
    }

    function verifiers() external view returns (address[] memory) {
        return _verifiers;
    }

    // Drivers and bridges ---------------------------------------------------

    function proposeDriver(bytes32 name, address implementation) external onlyAdmin {
        // Proposing a revoked implementation again is approving it again,
        // after a full timelock like anything else.
        uint64 readyAt = _propose(_driver[name], _driverRevoked[name], implementation);
        emit DriverProposed(name, implementation, readyAt);
    }

    function withdrawDriverProposal(bytes32 name) external onlyAdmin {
        address withdrawn = _withdraw(_driver[name], _driverRevoked[name]);
        emit DriverProposalWithdrawn(name, withdrawn);
    }

    function revokeDriver(bytes32 name, address implementation) external onlyAdmin {
        _revoke(_driver[name], _driverRevoked[name], implementation);
        emit DriverRevoked(name, implementation);
    }

    function tradingDriver(bytes32 name) external view returns (address) {
        return _trading(_driver[name], _driverRevoked[name]);
    }

    function isDriverRevoked(bytes32 name, address implementation) external view returns (bool) {
        return _isRevoked(_driver[name], _driverRevoked[name], implementation);
    }

    function proposeBridge(bytes32 name, address implementation) external onlyAdmin {
        uint64 readyAt = _propose(_bridge[name], _bridgeRevoked[name], implementation);
        emit BridgeProposed(name, implementation, readyAt);
    }

    function withdrawBridgeProposal(bytes32 name) external onlyAdmin {
        address withdrawn = _withdraw(_bridge[name], _bridgeRevoked[name]);
        emit BridgeProposalWithdrawn(name, withdrawn);
    }

    function revokeBridge(bytes32 name, address implementation) external onlyAdmin {
        _revoke(_bridge[name], _bridgeRevoked[name], implementation);
        emit BridgeRevoked(name, implementation);
    }

    function bridgeOf(bytes32 name) external view returns (address) {
        return _trading(_bridge[name], _bridgeRevoked[name]);
    }

    function isBridgeRevoked(bytes32 name, address implementation) external view returns (bool) {
        return _isRevoked(_bridge[name], _bridgeRevoked[name], implementation);
    }

    function _effective(Slot storage slot) private view returns (address) {
        if (slot.next != address(0) && block.timestamp >= slot.nextReadyAt) return slot.next;
        return slot.current;
    }

    /// What a station created now pins: the latest approval, unless it was
    /// revoked. A proposal that has waited out its timelock is a fresh
    /// approval, whatever it replaced.
    function _trading(
        Slot storage slot,
        mapping(address => bool) storage revoked
    ) private view returns (address implementation) {
        implementation = _effective(slot);
        if (_isRevoked(slot, revoked, implementation)) return address(0);
    }

    /// Whether `implementation` stands revoked. One proposed again and past
    /// its timelock is approved again, even before anything settles it.
    function _isRevoked(
        Slot storage slot,
        mapping(address => bool) storage revoked,
        address implementation
    ) private view returns (bool) {
        if (implementation == slot.next && slot.next != address(0) && block.timestamp >= slot.nextReadyAt) {
            return false;
        }
        return revoked[implementation];
    }

    function _settle(Slot storage slot, mapping(address => bool) storage revoked) private {
        if (slot.next != address(0) && block.timestamp >= slot.nextReadyAt) {
            delete revoked[slot.next];
            slot.current = slot.next;
            delete slot.next;
            delete slot.nextReadyAt;
        }
    }

    function _propose(
        Slot storage slot,
        mapping(address => bool) storage revoked,
        address implementation
    ) private returns (uint64 readyAt) {
        if (implementation == address(0)) revert NoImplementation();
        _settle(slot, revoked);
        // A second proposal before the first is ready replaces it, and waits
        // its own full timelock.
        readyAt = _readyAt();
        slot.next = implementation;
        slot.nextReadyAt = readyAt;
    }

    /// Withdraw the proposal still waiting out its timelock. What is in
    /// force, and every station that pinned it, is untouched.
    function _withdraw(
        Slot storage slot,
        mapping(address => bool) storage revoked
    ) private returns (address withdrawn) {
        _settle(slot, revoked);
        withdrawn = slot.next;
        if (withdrawn == address(0)) revert NoImplementation();
        delete slot.next;
        delete slot.nextReadyAt;
    }

    /// Revoke an implementation that was approved: the one in force, or an
    /// older one stations still pin. They stop trading through it at once
    /// and keep valuing and closing through it.
    function _revoke(
        Slot storage slot,
        mapping(address => bool) storage revoked,
        address implementation
    ) private {
        _settle(slot, revoked);
        if (implementation == address(0)) revert NoImplementation();
        // A proposal still waiting has not been approved: withdraw it.
        if (implementation == slot.next) revert NotApproved();
        revoked[implementation] = true;
    }

    // Blessing --------------------------------------------------------------

    function bless(
        bytes calldata seal,
        bytes calldata journalBytes,
        uint256 vaultChain,
        address vault,
        bytes32 strategyHash,
        bytes32[] calldata merkleProof
    ) external {
        IAttestationVerifier verifier = _verifierFor(seal, journalBytes);
        Journal memory journal = abi.decode(journalBytes, (Journal));

        if (!isImageAllowed(journal.imageDigest)) revert ImageNotAllowed();
        if (!isProjectAllowed(journal.projectId)) revert ProjectNotAllowed();
        ImageKind kind = _image[journal.imageDigest].kind;

        if (journal.signer == address(0)) revert NoSigner();
        bool servesOneVault = kind == ImageKind.Executor || kind == ImageKind.Unwinder;
        if (
            servesOneVault
                ? (vaultChain == 0 || vault == address(0) || strategyHash == 0)
                : (vaultChain != 0 || vault != address(0) || strategyHash != 0)
        ) {
            revert WrongKindForVault();
        }
        // The key may act for this vault and this bundle only, on this chain
        // only, and the instance had to have said so before it saw this
        // transaction. A builder or a reviewer proves the leaf with no vault.
        bytes32 leaf =
            keccak256(bytes.concat(keccak256(abi.encode(vaultChain, vault, strategyHash, block.chainid))));
        if (!MerkleProof.verify(merkleProof, journal.vaultsRoot, leaf)) revert NotAmongTheVaults();

        // A blessing runs from the attestation, so replaying an old one can
        // never extend it.
        uint64 until = journal.issuedAt + blessingTtl;
        if (journal.issuedAt > block.timestamp || until <= block.timestamp) revert Stale();

        KeyInfo storage key = _key[journal.signer];
        if (
            key.kind != ImageKind.None
                && (key.imageDigest != journal.imageDigest || key.projectId != journal.projectId)
        ) {
            revert NotThisInstance();
        }
        // A blessing only ever moves later, and remembers the checker that
        // gave it, so retiring that checker retires it.
        bytes32 target = _target(vaultChain, vault, strategyHash);
        Blessing storage blessing = _blessing[journal.signer][target];
        // Also when the checker it was recorded against has since been
        // retired: an allowed checker vouching for it again revives it.
        if (until > blessing.until || !isVerifierAllowed(blessing.verifier)) {
            blessing.until = until;
            blessing.verifier = address(verifier);
        }

        key.imageDigest = journal.imageDigest;
        key.projectId = journal.projectId;
        key.kind = kind;
        // What the key is reported as serving moves only with a later
        // blessing, so replaying a public seal can't change it.
        if (until > key.blessedUntil || !isVerifierAllowed(key.verifier)) {
            key.strategyHash = strategyHash;
            key.vaultChain = vaultChain;
            key.vault = vault;
            key.blessedUntil = until;
            key.trustRoot = verifier.trustRoot();
            key.verifier = address(verifier);
        }
        emit Blessed(journal.signer, vaultChain, vault, strategyHash, journal.imageDigest, blessing.until);
    }

    function _target(
        uint256 vaultChain,
        address vault,
        bytes32 strategyHash
    ) private pure returns (bytes32) {
        return keccak256(abi.encode(vaultChain, vault, strategyHash));
    }

    /// The newest allowed checker that accepts the proof. Newest first, so
    /// that during a rotation keys are recorded against the checker that will
    /// outlast the overlap.
    function _verifierFor(
        bytes calldata seal,
        bytes calldata journal
    ) private view returns (IAttestationVerifier) {
        for (uint256 i = _verifiers.length; i > 0; i--) {
            address verifier = _verifiers[i - 1];
            if (!isVerifierAllowed(verifier)) continue;
            // A checker that chokes on a proof meant for another one, or
            // answers in a shape nobody asked for, is just a checker that
            // didn't accept it.
            (bool ok, bytes memory answer) =
                verifier.staticcall(abi.encodeCall(IAttestationVerifier.verify, (seal, journal)));
            if (ok && answer.length == 32 && abi.decode(answer, (uint256)) == 1) {
                return IAttestationVerifier(verifier);
            }
        }
        revert ProofRejected();
    }

    function keyInfo(address signer) external view returns (KeyInfo memory) {
        return _key[signer];
    }

    function blessedUntil(
        address signer,
        uint256 vaultChain,
        address vault,
        bytes32 strategyHash
    ) external view returns (uint64) {
        return _blessing[signer][_target(vaultChain, vault, strategyHash)].until;
    }

    /// Whether the key's image and project are both still allowed. Revoking
    /// either reaches every key it holds at once.
    function _stillAllowed(KeyInfo storage key) private view returns (bool) {
        // The checker is checked per blessing, by the callers.
        return isImageAllowed(key.imageDigest) && isProjectAllowed(key.projectId);
    }

    function isBlessedFor(
        address signer,
        uint256 vaultChain,
        address vault,
        bytes32 strategyHash
    ) external view returns (bool) {
        if (vault == address(0)) return false;
        Blessing storage blessing = _blessing[signer][_target(vaultChain, vault, strategyHash)];
        return blessing.until >= block.timestamp && isVerifierAllowed(blessing.verifier)
            && _stillAllowed(_key[signer]);
    }

    function isBlessedAs(address signer, ImageKind kind) external view returns (bool) {
        KeyInfo storage key = _key[signer];
        return key.kind == kind && key.blessedUntil >= block.timestamp && isVerifierAllowed(key.verifier)
            && _stillAllowed(key);
    }
}
