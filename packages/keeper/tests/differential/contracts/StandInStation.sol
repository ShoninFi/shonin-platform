// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {
    IAttestationRegistry,
    ImageKind
} from "../../../../../contracts/evm/src/interfaces/IAttestationRegistry.sol";
import {IStation, IStationVault, Send, Stage, StationSpec} from "../../../../../contracts/evm/src/interfaces/IStation.sol";
import {Scripted} from "./Scripted.sol";

/// A home station as the keeper sees it, for the keeper's differential
/// tests: views from the script, and the keeper's calls with the effects a
/// station's would have on them. `revokeKey` refuses a key under the
/// station's own condition, read from the registry as the station reads it.
///
/// Script keys: `vault`, `registry`, `factory`, `vaultChain`, `bundleHash`,
/// `exitBandBps`, `driverCount`, `driver`/i, `pinned`/driver,
/// `approvedKey`/driver, `reclaimed`/driver, `heldInCount`/driver,
/// `heldIn`/driver/i, `stopped`, `stoppedAt`, `stoppedBand`, `value.mark`,
/// `value.exit`, `value.burn` (hashes `value()` spends first), `outsideBand`, `unwind.blocked`, `stage.*` (each field of
/// `Stage`), `stage.deliverAfter` (how long after its sweep a staged close
/// may be delivered).
contract StandInStation is Scripted, IStation {
    /// The key still stands: its blessing for the vault holds, it is an
    /// executor's, its driver trades, and the station runs.
    error StillBlessed();
    error WrongState();
    error Nothing();
    error NotYet();

    // Setup ------------------------------------------------------------------

    function emitRevokeSkipped(bytes32 driver, address key) external {
        if (msg.sender != scripter) revert NotTheScripter();
        emit RevokeSkipped(driver, key);
    }

    // What the keeper sends --------------------------------------------------

    function revokeKey(bytes32 driver, address key) external {
        IAttestationRegistry registry = IAttestationRegistry(_addr(_get("registry")));
        address implementation = pinnedDriver(driver);
        if (
            !stopped() && !reclaimed(driver)
                && registry.isBlessedFor(key, _get("vaultChain"), vault(), bytes32(_get("bundleHash")))
                && registry.keyInfo(key).kind == ImageKind.Executor
                && !registry.isDriverRevoked(driver, implementation)
        ) revert StillBlessed();
        if (approvedKey(driver) == key) _set("approvedKey", uint256(driver), 0);
        emit KeyRevoked(driver, key);
    }

    /// Anyone, once the vault has halted and its grace has passed.
    function stop() external {
        if (stopped()) revert WrongState();
        IStationVault home = IStationVault(vault());
        uint64 halted = home.haltedAt();
        if (halted == 0 || block.timestamp < uint256(halted) + home.haltGrace()) revert NotYet();
        _set("stopped", 1);
        _set("stoppedAt", block.timestamp);
        emit Stopped("halted", halted);
    }

    function unwind(uint256 fraction) external returns (uint256 sentHome, bool blocked) {
        return _unwind(fraction);
    }

    function unwindStopped(bytes32[] calldata) external returns (uint256 sentHome, bool blocked) {
        return _unwind(1e18);
    }

    function _unwind(uint256 fraction) private returns (uint256 sentHome, bool blocked) {
        if (!stopped()) revert WrongState();
        blocked = _get("unwind.blocked") != 0;
        if (!blocked) {
            sentHome = _get("value.mark");
            _set("value.mark", 0);
            _set("value.exit", 0);
        }
        emit Unwound(fraction, sentHome, blocked);
    }

    /// Prove and sweep the staged close once its settle delay has passed;
    /// deliver it once `stage.deliverAfter` has passed since the sweep.
    function settleClose() external {
        uint256 stagedAt = _get("stage.stagedAt");
        if (stagedAt == 0) revert Nothing();
        if (_get("stage.sweptBlock") == 0) {
            if (block.timestamp < stagedAt + _get("stage.delay")) revert NotYet();
            _set("stage.sweptAt", block.timestamp);
            _set("stage.sweptBlock", block.number);
            emit CloseSwept(_get("stage.fraction"), 0, 0);
            return;
        }
        if (block.timestamp < _get("stage.sweptAt") + _get("stage.deliverAfter")) revert NotYet();
        _set("stage.stagedAt", 0);
        _set("stage.sweptAt", 0);
        _set("stage.sweptBlock", 0);
        emit CloseDelivered(_get("stage.fraction"), 0);
    }

    function reclaim(bytes32 driver, address venueVault) external {
        uint256 count = _get("heldInCount", uint256(driver));
        for (uint256 i = 0; i < count; i++) {
            if (_addr(_get("heldIn", uint256(driver), i)) == venueVault) revert Nothing();
        }
        _set("heldIn", uint256(driver), count, _word(venueVault));
        _set("heldInCount", uint256(driver), count + 1);
        _set("reclaimed", uint256(driver), 1);
        emit Reclaimed(driver, venueVault, 0);
    }

    // What the keeper reads --------------------------------------------------

    /// The station factory, whose `attestation()` is the registry.
    function factory() external view returns (address) {
        return _addr(_get("factory"));
    }

    function vault() public view returns (address) {
        return _addr(_get("vault"));
    }

    function spec() external view returns (StationSpec memory s) {
        s.vaultChain = _get("vaultChain");
        s.vault = vault();
        s.bundleHash = bytes32(_get("bundleHash"));
        uint256 count = _get("driverCount");
        s.drivers = new bytes32[](count);
        for (uint256 i = 0; i < count; i++) {
            s.drivers[i] = bytes32(_get("driver", i));
        }
        s.exitBandBps = uint16(_get("exitBandBps"));
    }

    function stopped() public view returns (bool) {
        return _get("stopped") != 0;
    }

    function stoppedAt() external view returns (uint64) {
        return uint64(_get("stoppedAt"));
    }

    function stoppedBand() external view returns (uint16) {
        return uint16(_get("stoppedBand"));
    }

    function approvedKey(bytes32 driver) public view returns (address) {
        return _addr(_get("approvedKey", uint256(driver)));
    }

    function pinnedDriver(bytes32 driver) public view returns (address) {
        return _addr(_get("pinned", uint256(driver)));
    }

    function reclaimed(bytes32 driver) public view returns (bool) {
        return _get("reclaimed", uint256(driver)) != 0;
    }

    function heldIn(bytes32 driver) external view returns (address[] memory held) {
        uint256 count = _get("heldInCount", uint256(driver));
        held = new address[](count);
        for (uint256 i = 0; i < count; i++) {
            held[i] = _addr(_get("heldIn", uint256(driver), i));
        }
    }

    function value() external view returns (uint256 mark, uint256 exit) {
        _burn(_get("value.burn"));
        return (_get("value.mark"), _get("value.exit"));
    }

    /// Spend about `rounds` hashes' gas: a view too costly for one read of a
    /// batch (about 1.5M gas at 20,000).
    function _burn(uint256 rounds) private pure {
        uint256 word_;
        for (uint256 i = 0; i < rounds; i++) {
            word_ = uint256(keccak256(abi.encode(word_, i)));
        }
    }

    function outsideBand() external view returns (bool) {
        return _get("outsideBand") != 0;
    }

    function stage() external view returns (Stage memory s) {
        s.fraction = _get("stage.fraction");
        s.owed = _get("stage.owed");
        s.estimate = _get("stage.estimate");
        s.fetched = _get("stage.fetched");
        s.cost = _get("stage.cost");
        s.part = _get("stage.part");
        s.arrivedAt = _get("stage.arrivedAt");
        s.stagedAt = uint64(_get("stage.stagedAt"));
        s.stagedBlock = uint64(_get("stage.stagedBlock"));
        s.sweptAt = uint64(_get("stage.sweptAt"));
        s.sweptBlock = uint64(_get("stage.sweptBlock"));
        s.window = uint64(_get("stage.window"));
        s.delay = uint64(_get("stage.delay"));
        s.forVault = _get("stage.forVault") != 0;
    }

    function isHome() external pure returns (bool) {
        return true;
    }

    function arrivals() external pure returns (uint256) {
        return 0;
    }

    function venueFlows(bytes32) external pure returns (uint256, uint64) {}

    function movedAt(bytes32) external pure returns (uint64) {}

    function fullUnwindAt(bytes32) external pure returns (uint64) {}

    function pinnedBridge(bytes32) external pure returns (address) {}

    function WIDEN_AFTER() external pure returns (uint64) {
        return 3600;
    }

    function sends() external pure returns (Send[] memory) {}

    function sendCount() external pure returns (uint256) {}

    function sendAt(uint256) external pure returns (Send memory) {}

    function stopDigest(bytes32, uint64) external pure returns (bytes32) {}

    // Nothing in the tests calls these -------------------------------------

    function approveKey(bytes32, address) external pure {
        revert NotScripted();
    }

    function pull(uint256) external pure {
        revert NotScripted();
    }

    function push(uint256) external pure {
        revert NotScripted();
    }

    function deposit(bytes32, uint256) external pure {
        revert NotScripted();
    }

    function withdraw(bytes32, uint256) external pure {
        revert NotScripted();
    }

    function bridge(bytes32, uint256, uint256, bytes calldata) external pure {
        revert NotScripted();
    }

    function stop(bytes32, uint64, bytes calldata) external pure {
        revert NotScripted();
    }

    function stageClose(uint256) external pure returns (uint256, bool, bool) {
        revert NotScripted();
    }

    function revokeOnce(bytes32, address) external pure {
        revert NotScripted();
    }

    function closeAll(bytes32, uint16) external pure returns (bool) {
        revert NotScripted();
    }
}
