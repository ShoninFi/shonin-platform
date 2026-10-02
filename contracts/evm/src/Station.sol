// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationRegistry, ImageKind} from "./interfaces/IAttestationRegistry.sol";
import {Call, IBridge, IDriver} from "./interfaces/IDriver.sol";
import {IStation, IStationFactory, IStationVault, Send, StationSpec} from "./interfaces/IStation.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";

import {ReentrancyGuardTransient} from "openzeppelin/utils/ReentrancyGuardTransient.sol";
import {ECDSA} from "openzeppelin/utils/cryptography/ECDSA.sol";
import {MessageHashUtils} from "openzeppelin/utils/cryptography/MessageHashUtils.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// A vault's account-holder on one chain: the master of its venue accounts.
///
/// Every venue call is the station's own, prepared by a driver the registry
/// approves and checked here against that driver's targets before it is made.
/// A call to the dollar is only ever an approval of one of those targets, so
/// the station's dollars can only reach a target its driver declared.
///
/// It pins its drivers and bridges when it is created: the implementations
/// the registry approved then are the only ones it ever calls, so nothing
/// anybody approves later reaches it. Revoking a pinned driver stops trading
/// through it and leaves valuing and closing working.
/// Trading keys are the venue's agents: they can trade the account and can't
/// take from it. A bridge send names the station's own address as recipient,
/// which is the same address on every chain.
///
/// It has no signature function of its own, so nothing a venue accepts from
/// an account's owner by signature can ever come from a station.
contract Station is IStation, ReentrancyGuardTransient {
    using SafeERC20 for IERC20;
    using Math for uint256;

    uint256 private constant WAD = 1e18;
    uint256 private constant BPS = 10_000;
    /// A stopped station's full unwind keeps the exit band this long after
    /// the stop, then the band doubles each time this passes again.
    uint64 public constant WIDEN_AFTER = 7 days;
    /// What a best-effort revoke may spend in all: far more than a venue's
    /// revoke costs, far less than a block.
    uint256 private constant BEST_EFFORT_GAS = 2_000_000;
    /// The most calls a driver may prepare to revoke a key.
    uint256 private constant MAX_REVOKE_CALLS = 8;
    bytes4 private constant APPROVE = IERC20.approve.selector;

    IStationFactory public factory;
    StationSpec private _spec;
    bool public stopped;
    mapping(bytes32 => address) private _approvedKey;
    Send[] private _sends;
    mapping(bytes32 => address) private _pinnedDriver;
    mapping(bytes32 => address) private _pinnedBridge;
    uint64 public stoppedAt;

    error AlreadyInitialized();
    error NotBlessed();
    error NotHome();
    error IsHome();
    error AlreadyStopped();
    error NotDeclared();
    error NotApproved();
    error NotAllowed();
    error StillBlessed();
    error NotHalted();
    error BadCall();
    error NoRoute();

    /// The implementation every station is a clone of is never a station
    /// itself.
    constructor() {
        factory = IStationFactory(address(1));
    }

    function initialize(StationSpec calldata spec_) external {
        if (address(factory) != address(0)) revert AlreadyInitialized();
        factory = IStationFactory(msg.sender);
        // Each driver and bridge once: a name listed twice would count its
        // venue twice and close a fraction of it twice.
        _unique(spec_.drivers);
        _unique(spec_.bridges);
        _spec = spec_;
        // Pin what the registry approves now, for good.
        IAttestationRegistry registry = _registry();
        for (uint256 i = 0; i < spec_.drivers.length; i++) {
            address implementation = registry.tradingDriver(spec_.drivers[i]);
            if (implementation == address(0)) revert NotApproved();
            _pinnedDriver[spec_.drivers[i]] = implementation;
        }
        for (uint256 i = 0; i < spec_.bridges.length; i++) {
            address implementation = registry.bridgeOf(spec_.bridges[i]);
            if (implementation == address(0)) revert NotApproved();
            _pinnedBridge[spec_.bridges[i]] = implementation;
        }
    }

    // Keys ------------------------------------------------------------------

    function approveKey(bytes32 driver, address key) external nonReentrant {
        if (stopped) revert AlreadyStopped();
        _declaredDriver(driver);
        if (!_isKey(key, ImageKind.Executor)) revert NotBlessed();
        address implementation = _tradable(driver);

        // One executor at a time: the key in place is withdrawn first, so an
        // instance that has been replaced can no longer trade.
        address current = _approvedKey[driver];
        // While the key in place is still blessed, only the key being
        // approved may take its place: nobody else can hand the venue back
        // and forth between two blessed instances during a handover.
        if (
            current != address(0) && current != key && msg.sender != key
                && _isKey(current, ImageKind.Executor)
        ) {
            revert NotAllowed();
        }
        if (current != address(0) && current != key) {
            // A key still blessed must be withdrawn; one whose blessing has
            // lapsed is withdrawn if the driver can, and replaced either way.
            if (_isKey(current, ImageKind.Executor)) {
                _revokeStrict(driver, current, false);
                emit KeyRevoked(driver, current);
            } else if (_revokeBestEffort(driver, current)) {
                emit KeyRevoked(driver, current);
            } else {
                // Anyone may still withdraw it, strictly, with `revokeKey`.
                emit RevokeSkipped(driver, current);
            }
        }
        uint64 until = _registry().blessedUntil(key, _spec.vaultChain, _spec.vault, _spec.bundleHash);
        _run(IDriver(implementation).approveKey(address(this), key, until), IDriver(implementation).targets());
        _approvedKey[driver] = key;
        emit KeyApproved(driver, key, until);
    }

    function revokeKey(bytes32 driver, address key) external nonReentrant {
        // A passer-by can't cut off a key whose blessing stands, and can't
        // keep one whose blessing has gone.
        // Once the driver no longer trades, its key goes too.
        address implementation = _pinned(driver);
        if (
            !stopped && _isKey(key, ImageKind.Executor)
                && !_registry().isDriverRevoked(driver, implementation)
        ) {
            revert StillBlessed();
        }
        _revoke(driver, key);
    }

    // Moving money ----------------------------------------------------------

    function pull(uint256 amount) external nonReentrant {
        _executorOnly();
        if (!isHome()) revert NotHome();
        IStationVault(_spec.vault).toStation(amount);
        emit Pulled(amount);
    }

    function push(uint256 amount) external nonReentrant {
        _executorOnly();
        if (!isHome()) revert NotHome();
        _sendHome(amount);
        emit Pushed(amount);
    }

    function deposit(bytes32 driver, uint256 amount) external nonReentrant {
        _executorOnly();
        address implementation = _tradable(driver);
        _run(IDriver(implementation).deposit(address(this), amount), IDriver(implementation).targets());
        emit Deposited(driver, amount);
    }

    function withdraw(bytes32 driver, uint256 amount) external nonReentrant {
        _executorOnly();
        // Bringing money out never depends on the driver still being
        // approved to trade.
        address implementation = _pinned(driver);
        _run(IDriver(implementation).withdraw(address(this), amount), IDriver(implementation).targets());
        emit Withdrawn(driver, amount);
    }

    function bridge(
        bytes32 name,
        uint256 toChain,
        uint256 amount,
        bytes calldata extra
    ) external nonReentrant {
        _executorOnly();
        if (toChain == block.chainid) revert NoRoute();
        // Nothing leaves the home station while a valuation waits for the
        // other chains' posts: it was counted here at the open, and landing
        // there before their posts it would be counted twice.
        if (isHome() && IStationVault(_spec.vault).valuing()) revert NotAllowed();
        _send(name, toChain, amount, extra);
    }

    // Stopping and unwinding ------------------------------------------------

    function stop() external nonReentrant {
        if (!isHome()) revert NotHome();
        uint64 haltedAt = IStationVault(_spec.vault).haltedAt();
        if (haltedAt == 0 || block.timestamp < haltedAt + IStationVault(_spec.vault).haltGrace()) {
            revert NotHalted();
        }
        _stop("halted", haltedAt);
    }

    function stop(bytes32 reason, uint64 haltedAt, bytes calldata signature) external nonReentrant {
        if (isHome()) revert IsHome();
        bytes32 digest = MessageHashUtils.toEthSignedMessageHash(stopDigest(reason, haltedAt));
        (address signer, ECDSA.RecoverError err,) = ECDSA.tryRecover(digest, signature);
        if (err != ECDSA.RecoverError.NoError) revert NotBlessed();
        if (!_isKey(signer, ImageKind.Executor) && !_isKey(signer, ImageKind.Unwinder)) revert NotBlessed();
        _stop(reason, haltedAt);
    }

    function unwind(uint256 fraction) external nonReentrant returns (uint256 sentHome, bool blocked) {
        if (fraction == 0 || fraction > WAD) revert NotAllowed();
        // Once stopped, a fraction is closed only for the vault's own claims
        // and serves, which wait for settlement themselves; the rest is a
        // full unwind, which `_unwindStopped` holds until settlement too.
        bool permitted = (isHome() && msg.sender == _spec.vault)
            || (!stopped && (_isKey(msg.sender, ImageKind.Executor) || _isKey(msg.sender, ImageKind.Unwinder)))
            || (stopped && fraction == WAD);
        if (!permitted) revert NotAllowed();
        if (stopped && fraction == WAD) return _unwindStopped(_spec.drivers);

        // Every driver's close is prepared before any is made, so a market
        // outside the band on one venue closes nothing anywhere: a leaver is
        // never half-sold.
        uint256 count = _spec.drivers.length;
        IDriver[] memory drivers = new IDriver[](count);
        Call[][] memory closes = new Call[][](count);
        uint256[] memory freed = new uint256[](count);
        bool[] memory skipped = new bool[](count);
        for (uint256 i = 0; i < count; i++) {
            drivers[i] = IDriver(_pinned(_spec.drivers[i]));
            // What the fraction is worth at exit prices before the close:
            // exactly what selling it realises, and all that comes home for
            // it. The rest stays with whoever still owns it.
            (uint256 markBefore, uint256 exitBefore) = drivers[i].valueOf(address(this));
            // The band first, for every account: a market outside it closes
            // nothing anywhere, whatever any account reads as.
            bool outside;
            (closes[i], outside) = drivers[i].close(address(this), fraction, _spec.exitBandBps);
            if (outside) {
                emit Unwound(fraction, 0, true);
                return (0, true);
            }
            // An account worth no more than a few dollar-units is dust: a
            // fraction of it is left where it is rather than let its rounding
            // hold every leaver's close. A full unwind still sells it.
            if (fraction < WAD && markBefore <= count + 1) {
                skipped[i] = true;
                continue;
            }
            freed[i] = exitBefore.mulDiv(fraction, WAD, Math.Rounding.Floor);
            // A fraction worth more than dust at the mark and nothing at exit
            // prices (an account under water at the bid) is never sold for
            // the leaver: it would bring them nothing and leave the loss with
            // everyone else. Only a full unwind, which everyone shares, may.
            if (freed[i] == 0 && markBefore.mulDiv(fraction, WAD) > count + 1 && fraction < WAD) {
                revert ShortSweep();
            }
        }

        IERC20 dollar = _asset();
        uint256 idleBefore = dollar.balanceOf(address(this));
        uint256 brought;
        for (uint256 i = 0; i < count; i++) {
            if (skipped[i]) continue;
            address[] memory targets = drivers[i].targets();
            uint256 before = dollar.balanceOf(address(this));
            _run(closes[i], targets);
            _run(drivers[i].sweep(address(this), freed[i]), targets);
            // Selling the fraction is only a serve if its worth comes home:
            // proceeds left at the venue would still belong to everyone.
            uint256 swept = dollar.balanceOf(address(this)) - before;
            if (swept < freed[i].mulDiv(BPS - _spec.exitBandBps, BPS)) revert ShortSweep();
            // Never more than the fraction's worth: anything a venue sends
            // beyond it stays here, everyone's.
            brought += swept < freed[i] ? swept : freed[i];
        }
        sentHome = brought + idleBefore.mulDiv(fraction, WAD, Math.Rounding.Floor);

        if (sentHome != 0) {
            if (isHome()) {
                _sendHome(sentHome);
            } else if (_spec.bridges.length != 0) {
                _send(_spec.bridges[0], _spec.vaultChain, sentHome, "");
            } else {
                revert NoRoute();
            }
        }
        emit Unwound(fraction, sentHome, false);
    }

    /// A stopped station's full unwind, which every holder shares: each
    /// driver closes on its own, inside the band as it has widened since the
    /// stop, so a venue that refuses or a market still outside the band
    /// leaves only its own money behind. Everything idle then goes home.
    function _unwindStopped(bytes32[] memory drivers) private returns (uint256 sentHome, bool blocked) {
        // Not between a valuation's pricing and its settlement: what this
        // sale brings home must not pay withdrawals priced before it.
        if (isHome() && (IStationVault(_spec.vault).valuing() || IStationVault(_spec.vault).unsettled())) {
            revert NotAllowed();
        }
        uint16 band = stoppedBand();
        for (uint256 i = 0; i < drivers.length; i++) {
            _pinned(drivers[i]);
            try this.closeAll(drivers[i], band) returns (bool outside) {
                if (outside) blocked = true;
            } catch {
                blocked = true;
                emit DriverFailed(drivers[i]);
            }
        }
        sentHome = _asset().balanceOf(address(this));
        if (sentHome != 0) {
            if (isHome()) {
                _sendHome(sentHome);
            } else if (_spec.bridges.length != 0) {
                _send(_spec.bridges[0], _spec.vaultChain, sentHome, "");
            } else {
                revert NoRoute();
            }
        }
        emit Unwound(WAD, sentHome, blocked);
    }

    /// A stopped station's full unwind through the drivers named, then
    /// everything idle home; with none named, only the idle dollars. Anyone
    /// may call it, so a driver that burns its gas can be left out and can't
    /// keep the other venues' money, or the idle dollars, from coming home.
    function unwindStopped(bytes32[] calldata drivers)
        external
        nonReentrant
        returns (uint256 sentHome, bool blocked)
    {
        if (!stopped) revert NotHalted();
        return _unwindStopped(drivers);
    }

    /// One driver's part of a stopped station's full unwind. Only the
    /// station calls it, from `unwind`, so one driver's failure undoes only
    /// its own part.
    function closeAll(bytes32 name, uint16 band) external returns (bool outside) {
        if (msg.sender != address(this)) revert NotAllowed();
        IDriver driver = IDriver(_pinned(name));
        (, uint256 exitBefore) = driver.valueOf(address(this));
        Call[] memory calls;
        (calls, outside) = driver.close(address(this), WAD, band);
        if (outside) return true;
        address[] memory targets = driver.targets();
        IERC20 dollar = _asset();
        uint256 before = dollar.balanceOf(address(this));
        _run(calls, targets);
        _run(driver.sweep(address(this), exitBefore), targets);
        uint256 swept = dollar.balanceOf(address(this)) - before;
        if (swept < exitBefore.mulDiv(BPS - band, BPS)) revert ShortSweep();
    }

    // What it holds ---------------------------------------------------------

    function value() external view returns (uint256 mark, uint256 exit) {
        uint256 idle = _asset().balanceOf(address(this));
        mark = idle;
        exit = idle;
        for (uint256 i = 0; i < _spec.drivers.length; i++) {
            // Valued through the driver it can unwind through, so a revoked
            // driver's holdings still count.
            (uint256 m, uint256 x) = IDriver(_pinned(_spec.drivers[i])).valueOf(address(this));
            mark += m;
            exit += x;
        }
    }

    function outsideBand() external view returns (bool) {
        for (uint256 i = 0; i < _spec.drivers.length; i++) {
            (, bool blocked) = IDriver(_pinned(_spec.drivers[i])).close(address(this), WAD, _spec.exitBandBps);
            if (blocked) return true;
        }
        return false;
    }

    function spec() external view returns (StationSpec memory) {
        return _spec;
    }

    function isHome() public view returns (bool) {
        return _spec.vaultChain == block.chainid;
    }

    function pinnedDriver(bytes32 driver) external view returns (address) {
        return _pinnedDriver[driver];
    }

    function pinnedBridge(bytes32 name) external view returns (address) {
        return _pinnedBridge[name];
    }

    /// The band a stopped station's full unwind closes inside: the exit band
    /// for `WIDEN_AFTER` after the stop, then doubling each `WIDEN_AFTER`
    /// until any price is accepted. No key sets it; only time does.
    function stoppedBand() public view returns (uint16) {
        uint256 band = _spec.exitBandBps;
        if (!stopped || block.timestamp < uint256(stoppedAt) + WIDEN_AFTER) return uint16(band);
        uint256 doublings = (block.timestamp - stoppedAt) / WIDEN_AFTER;
        if (band == 0) band = 1;
        // 2^14 bps is past any price, so stop doubling there.
        if (doublings >= 14) return uint16(BPS);
        band <<= doublings;
        return uint16(band > BPS ? BPS : band);
    }

    function approvedKey(bytes32 driver) external view returns (address) {
        return _approvedKey[driver];
    }

    function sends() external view returns (Send[] memory) {
        return _sends;
    }

    function sendCount() external view returns (uint256) {
        return _sends.length;
    }

    function sendAt(uint256 index) external view returns (Send memory) {
        return _sends[index];
    }

    function stopDigest(bytes32 reason, uint64 haltedAt) public view returns (bytes32) {
        return keccak256(abi.encode(block.chainid, address(this), "shonin-stop", reason, haltedAt));
    }

    // Internals -------------------------------------------------------------

    function _stop(bytes32 reason, uint64 haltedAt) private {
        if (stopped) revert AlreadyStopped();
        stopped = true;
        stoppedAt = uint64(block.timestamp);
        for (uint256 i = 0; i < _spec.drivers.length; i++) {
            bytes32 driver = _spec.drivers[i];
            address key = _approvedKey[driver];
            if (key == address(0)) continue;
            // A venue or a driver that refuses the revoke can't keep the
            // station from stopping. A key whose revoke didn't go through
            // stays recorded, and anyone may withdraw it with `revokeKey`.
            if (_revokeBestEffort(driver, key)) {
                delete _approvedKey[driver];
                emit KeyRevoked(driver, key);
            } else {
                emit RevokeSkipped(driver, key);
            }
        }
        emit Stopped(reason, haltedAt);
    }

    function _revoke(bytes32 driver, address key) private {
        _revokeStrict(driver, key, false);
        if (_approvedKey[driver] == key) delete _approvedKey[driver];
        emit KeyRevoked(driver, key);
    }

    /// Withdraw `key` at the venue through the pinned driver; every call
    /// must go through.
    function _revokeStrict(bytes32 driver, address key, bool capped) private {
        IDriver implementation = IDriver(_pinned(driver));
        Call[] memory calls = implementation.revokeKey(address(this), key);
        if (capped && calls.length > MAX_REVOKE_CALLS) revert BadCall();
        _run(calls, implementation.targets());
    }

    /// The same, inside a self-call with one fixed budget: whatever the
    /// driver or the venue does (burns its gas, returns a list that doesn't
    /// decode, refuses), the station carries on, and learns whether the key
    /// is really gone.
    function _revokeBestEffort(bytes32 driver, address key) private returns (bool done) {
        try this.revokeOnce{gas: BEST_EFFORT_GAS}(driver, key) {
            done = true;
        } catch {}
    }

    function revokeOnce(bytes32 driver, address key) external {
        if (msg.sender != address(this)) revert NotAllowed();
        _revokeStrict(driver, key, true);
    }

    function _send(bytes32 name, uint256 toChain, uint256 amount, bytes memory extra) private {
        if (stopped && toChain != _spec.vaultChain) revert AlreadyStopped();
        // Only to a chain the vault trades on, where a station counts it; and
        // a station on another chain sends only home, so money between two
        // other chains goes through home, where every send is counted
        // against one moment, the valuation's open.
        if (toChain != _spec.vaultChain && (!isHome() || !_listed(_spec.chains, toChain))) revert NoRoute();
        if (!_contains(_spec.bridges, name)) revert NotDeclared();
        address implementation = _pinnedBridge[name];
        if (_registry().isBridgeRevoked(name, implementation)) revert NotApproved();
        // The recipient is this station's own address, which is the vault's
        // station on every chain, so a send can only reach the vault's own.
        _run(
            IBridge(implementation).send(address(this), toChain, address(this), amount, extra),
            IBridge(implementation).targets()
        );
        _sends.push(Send({toChain: toChain, amount: amount, sentAt: uint64(block.timestamp)}));
        // Numbered from zero, as `sendAt` and a post's in-flight list name it.
        emit Bridged(name, _sends.length - 1, toChain, amount);
    }

    function _sendHome(uint256 amount) private {
        _asset().forceApprove(_spec.vault, amount);
        IStationVault(_spec.vault).fromStation(amount);
    }

    /// Make each call after checking where it goes: to one of the driver's
    /// targets, or to the dollar only to approve one of them.
    /// Nothing a driver prepares reaches the vault, the station itself, its
    /// factory or the registry, as a target or as a spender, whatever the
    /// driver lists; a call the station would refuse is never made.
    function _run(Call[] memory calls, address[] memory targets) private {
        address dollar = address(_asset());
        for (uint256 i = 0; i < calls.length; i++) {
            Call memory c = calls[i];
            bool allowed;
            if (c.target == dollar) {
                if (c.data.length == 68 && bytes4(c.data) == APPROVE) {
                    (address spender,) = abi.decode(_args(c.data), (address, uint256));
                    allowed = _includes(targets, spender) && !_ours(spender);
                }
            } else {
                allowed = _includes(targets, c.target) && !_ours(c.target);
            }
            if (!allowed) revert BadCall();
            (bool ok, bytes memory reason) = c.target.call{value: c.value}(c.data);
            if (!ok) {
                assembly {
                    revert(add(reason, 32), mload(reason))
                }
            }
        }
    }

    function _ours(address target) private view returns (bool) {
        return target == _spec.vault || target == address(this) || target == address(factory)
            || target == address(_registry());
    }

    function _args(bytes memory data) private pure returns (bytes memory args) {
        args = new bytes(data.length - 4);
        for (uint256 i = 0; i < args.length; i++) {
            args[i] = data[i + 4];
        }
    }

    function _executorOnly() private view {
        if (stopped) revert AlreadyStopped();
        if (!_isKey(msg.sender, ImageKind.Executor)) revert NotBlessed();
    }

    /// A key blessed, on this chain, for this vault and its bundle, from an
    /// image of that kind.
    function _isKey(address key, ImageKind kind) private view returns (bool) {
        IAttestationRegistry registry = _registry();
        return registry.isBlessedFor(key, _spec.vaultChain, _spec.vault, _spec.bundleHash)
            && registry.keyInfo(key).kind == kind;
    }

    function _declaredDriver(bytes32 driver) private view {
        if (!_contains(_spec.drivers, driver)) revert NotDeclared();
    }

    /// The implementation pinned at creation for a declared driver.
    function _pinned(bytes32 driver) private view returns (address implementation) {
        implementation = _pinnedDriver[driver];
        if (implementation == address(0)) revert NotDeclared();
    }

    /// The pinned implementation, if it may still trade.
    function _tradable(bytes32 driver) private view returns (address implementation) {
        implementation = _pinned(driver);
        if (_registry().isDriverRevoked(driver, implementation)) revert NotApproved();
    }

    function _registry() private view returns (IAttestationRegistry) {
        return IAttestationRegistry(factory.attestation());
    }

    function _asset() private view returns (IERC20) {
        return IERC20(factory.asset());
    }

    function _listed(uint256[] storage list, uint256 item) private view returns (bool) {
        for (uint256 i = 0; i < list.length; i++) {
            if (list[i] == item) return true;
        }
        return false;
    }

    function _unique(bytes32[] calldata list) private pure {
        for (uint256 i = 0; i < list.length; i++) {
            for (uint256 j = i + 1; j < list.length; j++) {
                if (list[i] == list[j]) revert NotAllowed();
            }
        }
    }

    function _contains(bytes32[] storage list, bytes32 item) private view returns (bool) {
        for (uint256 i = 0; i < list.length; i++) {
            if (list[i] == item) return true;
        }
        return false;
    }

    function _includes(address[] memory list, address item) private pure returns (bool) {
        for (uint256 i = 0; i < list.length; i++) {
            if (list[i] == item) return true;
        }
        return false;
    }
}
