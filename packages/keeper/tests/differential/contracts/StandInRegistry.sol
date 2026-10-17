// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {
    IAttestationRegistry,
    Image,
    ImageKind,
    KeyInfo,
    RootKeyRecord
} from "../../../../../contracts/evm/src/interfaces/IAttestationRegistry.sol";
import {Scripted} from "./Scripted.sol";

/// The attestation registry as the keeper and the stations read it, for the
/// keeper's differential tests. A key's blessing is scripted per key (one
/// vault each in these tests), and `bless` extends it to the journal's issue
/// time plus the TTL, as the registry's does. No seal is checked.
///
/// Script keys: `ttl`, `until`/key, `kind`/key, `uncounted`/key (the
/// checker its blessing was recorded against has retired),
/// `driverRevoked`/name/implementation, `verifier`, and the root record:
/// `root.signer`, `root.issuedAt`, `root.verifier`, `root.setAt`.
contract StandInRegistry is Scripted, IAttestationRegistry {
    error BadJournal();
    error AnotherRoot();
    error Lapsed();

    function bless(
        bytes calldata,
        bytes calldata journal,
        uint256 vaultChain,
        address vault,
        bytes32 strategyHash,
        address key_,
        bytes32[] calldata
    ) external {
        uint64 until = _issuedAt(journal) + blessingTtl();
        if (until <= block.timestamp) revert Lapsed();
        if (until > _get("until", _word(key_))) _set("until", _word(key_), until);
        _set("kind", _word(key_), uint256(ImageKind.Executor));
        emit Blessed(key_, vaultChain, vault, strategyHash, bytes32(0), until);
    }

    function setRootKey(bytes calldata, bytes calldata journal, address rootSigner, bytes32[] calldata) external {
        address recorded = _addr(_get("root.signer"));
        if (recorded != address(0) && recorded != rootSigner) revert AnotherRoot();
        uint64 issuedAt = _issuedAt(journal);
        if (uint256(issuedAt) + blessingTtl() <= block.timestamp) revert Lapsed();
        _set("root.signer", _word(rootSigner));
        _set("root.issuedAt", issuedAt);
        _set("root.verifier", _get("verifier"));
        _set("root.setAt", block.timestamp);
        emit RootKeySet(rootSigner, bytes32(0), bytes32(0), _addr(_get("verifier")), issuedAt);
    }

    /// The journal's last word: when the instance attested.
    function _issuedAt(bytes calldata journal) private pure returns (uint64) {
        if (journal.length != 160) revert BadJournal();
        return uint64(uint256(bytes32(journal[128:160])));
    }

    function blessingTtl() public view returns (uint64) {
        return uint64(_get("ttl"));
    }

    function blessedUntil(address signer, uint256, address, bytes32) public view returns (uint64) {
        return uint64(_get("until", _word(signer)));
    }

    function isBlessedFor(address signer, uint256, address, bytes32) public view returns (bool) {
        return _get("until", _word(signer)) > block.timestamp && _get("uncounted", _word(signer)) == 0;
    }

    function isBlessedAs(address signer, ImageKind kind) external view returns (bool) {
        return uint256(kind) == _get("kind", _word(signer)) && isBlessedFor(signer, 0, address(0), 0);
    }

    function keyInfo(address signer) external view returns (KeyInfo memory info) {
        info.kind = ImageKind(_get("kind", _word(signer)));
        info.verifier = _addr(_get("verifier"));
        info.blessedUntil = uint64(_get("until", _word(signer)));
    }

    function isDriverRevoked(bytes32 name, address implementation) external view returns (bool) {
        return _get("driverRevoked", uint256(name), _word(implementation)) != 0;
    }

    function rootKey() external view returns (RootKeyRecord memory record) {
        record.signer = _addr(_get("root.signer"));
        record.issuedAt = uint64(_get("root.issuedAt"));
        record.verifier = _addr(_get("root.verifier"));
        record.setAt = uint64(_get("root.setAt"));
    }

    function isRootKeyLive() external view returns (bool) {
        return _get("root.signer") != 0;
    }

    function ROOT_KEY_PURPOSE() external pure returns (bytes32) {
        return keccak256("root");
    }

    function isImageAllowed(bytes32) external pure returns (bool) {
        return true;
    }

    function isProjectAllowed(bytes32) external pure returns (bool) {
        return true;
    }

    function isVerifierAllowed(address verifier) external view returns (bool) {
        return verifier == _addr(_get("verifier"));
    }

    function verifiers() external view returns (address[] memory listed) {
        listed = new address[](1);
        listed[0] = _addr(_get("verifier"));
    }

    function imageOf(bytes32) external pure returns (Image memory image) {
        image.kind = ImageKind.Executor;
    }

    function tradingDriver(bytes32) external pure returns (address) {}

    function bridgeOf(bytes32) external pure returns (address) {}

    function isBridgeRevoked(bytes32, address) external pure returns (bool) {}

    function timelock() external pure returns (uint64) {}

    function pendingTimelock() external pure returns (uint64, uint64) {}

    function admin() external pure returns (address) {}

    // Nothing in the tests calls these -------------------------------------

    function clearRootKey() external pure {
        revert NotScripted();
    }

    function proposeImage(bytes32, ImageKind) external pure {
        revert NotScripted();
    }

    function revokeImage(bytes32) external pure {
        revert NotScripted();
    }

    function proposeProject(bytes32) external pure {
        revert NotScripted();
    }

    function revokeProject(bytes32) external pure {
        revert NotScripted();
    }

    function proposeVerifier(address) external pure {
        revert NotScripted();
    }

    function retireVerifier(address) external pure {
        revert NotScripted();
    }

    function proposeDriver(bytes32, address) external pure {
        revert NotScripted();
    }

    function withdrawDriverProposal(bytes32) external pure {
        revert NotScripted();
    }

    function revokeDriver(bytes32, address) external pure {
        revert NotScripted();
    }

    function proposeBridge(bytes32, address) external pure {
        revert NotScripted();
    }

    function withdrawBridgeProposal(bytes32) external pure {
        revert NotScripted();
    }

    function revokeBridge(bytes32, address) external pure {
        revert NotScripted();
    }

    function setTimelock(uint64) external pure {
        revert NotScripted();
    }
}
