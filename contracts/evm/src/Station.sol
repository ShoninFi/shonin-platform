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
/// no driver, however wrong, can send the station's dollars anywhere else.
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
    bytes4 private constant APPROVE = IERC20.approve.selector;

    IStationFactory public factory;
    StationSpec private _spec;
    bool public stopped;
    mapping(bytes32 => address) private _approvedKey;
    Send[] private _sends;

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
        _spec = spec_;
    }

    // Keys ------------------------------------------------------------------

    function approveKey(bytes32 driver, address key) external nonReentrant {
        if (stopped) revert AlreadyStopped();
        _declaredDriver(driver);
        if (!_isKey(key, ImageKind.Executor)) revert NotBlessed();
        address implementation = _registry().tradingDriver(driver);
        if (implementation == address(0)) revert NotApproved();

        // One executor at a time: the key in place is withdrawn first, so an
        // instance that has been replaced can no longer trade.
        address current = _approvedKey[driver];
        if (current != address(0) && current != key) {
            _run(IDriver(implementation).revokeKey(address(this), current), IDriver(implementation).targets());
            emit KeyRevoked(driver, current);
        }
        uint64 until = _registry().blessedUntil(key, _spec.vaultChain, _spec.vault, _spec.bundleHash);
        _run(IDriver(implementation).approveKey(address(this), key, until), IDriver(implementation).targets());
        _approvedKey[driver] = key;
        emit KeyApproved(driver, key, until);
    }

    function revokeKey(bytes32 driver, address key) external nonReentrant {
        // A passer-by can't cut off a key whose blessing stands, and can't
        // keep one whose blessing has gone.
        if (!stopped && _isKey(key, ImageKind.Executor)) revert StillBlessed();
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
        _declaredDriver(driver);
        address implementation = _registry().tradingDriver(driver);
        if (implementation == address(0)) revert NotApproved();
        _run(IDriver(implementation).deposit(address(this), amount), IDriver(implementation).targets());
        emit Deposited(driver, amount);
    }

    function withdraw(bytes32 driver, uint256 amount) external nonReentrant {
        _executorOnly();
        _declaredDriver(driver);
        // Bringing money out never depends on the driver still being
        // approved to trade.
        address implementation = _unwindDriver(driver);
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
        bool permitted = (isHome() && msg.sender == _spec.vault) || _isKey(msg.sender, ImageKind.Executor)
            || _isKey(msg.sender, ImageKind.Unwinder) || (stopped && fraction == WAD);
        if (!permitted) revert NotAllowed();

        // Every driver's close is prepared before any is made, so a market
        // outside the band on one venue closes nothing anywhere: a leaver is
        // never half-sold.
        uint256 count = _spec.drivers.length;
        IDriver[] memory drivers = new IDriver[](count);
        Call[][] memory closes = new Call[][](count);
        for (uint256 i = 0; i < count; i++) {
            drivers[i] = IDriver(_unwindDriver(_spec.drivers[i]));
            bool outside;
            (closes[i], outside) = drivers[i].close(address(this), fraction, _spec.exitBandBps);
            if (outside) {
                emit Unwound(fraction, 0, true);
                return (0, true);
            }
        }

        IERC20 dollar = _asset();
        uint256 idleBefore = dollar.balanceOf(address(this));
        for (uint256 i = 0; i < count; i++) {
            address[] memory targets = drivers[i].targets();
            _run(closes[i], targets);
            _run(drivers[i].sweep(address(this), fraction), targets);
        }
        uint256 freed = dollar.balanceOf(address(this)) - idleBefore;
        sentHome = freed + idleBefore.mulDiv(fraction, WAD, Math.Rounding.Floor);

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

    // What it holds ---------------------------------------------------------

    function value() external view returns (uint256 mark, uint256 exit) {
        uint256 idle = _asset().balanceOf(address(this));
        mark = idle;
        exit = idle;
        for (uint256 i = 0; i < _spec.drivers.length; i++) {
            // Valued through the driver it can unwind through, so a revoked
            // driver's holdings still count.
            (uint256 m, uint256 x) = IDriver(_unwindDriver(_spec.drivers[i])).valueOf(address(this));
            mark += m;
            exit += x;
        }
    }

    function spec() external view returns (StationSpec memory) {
        return _spec;
    }

    function isHome() public view returns (bool) {
        return _spec.vaultChain == block.chainid;
    }

    function approvedKey(bytes32 driver) external view returns (address) {
        return _approvedKey[driver];
    }

    function sends() external view returns (Send[] memory) {
        return _sends;
    }

    function stopDigest(bytes32 reason, uint64 haltedAt) public view returns (bytes32) {
        return keccak256(abi.encode(block.chainid, address(this), "shonin-stop", reason, haltedAt));
    }

    // Internals -------------------------------------------------------------

    function _stop(bytes32 reason, uint64 haltedAt) private {
        if (stopped) revert AlreadyStopped();
        stopped = true;
        for (uint256 i = 0; i < _spec.drivers.length; i++) {
            address key = _approvedKey[_spec.drivers[i]];
            if (key != address(0)) _revoke(_spec.drivers[i], key);
        }
        emit Stopped(reason, haltedAt);
    }

    function _revoke(bytes32 driver, address key) private {
        _declaredDriver(driver);
        address implementation = _unwindDriver(driver);
        _run(IDriver(implementation).revokeKey(address(this), key), IDriver(implementation).targets());
        if (_approvedKey[driver] == key) delete _approvedKey[driver];
        emit KeyRevoked(driver, key);
    }

    function _send(bytes32 name, uint256 toChain, uint256 amount, bytes memory extra) private {
        if (stopped && toChain != _spec.vaultChain) revert AlreadyStopped();
        if (!_contains(_spec.bridges, name)) revert NotDeclared();
        address implementation = _registry().bridgeOf(name);
        if (implementation == address(0)) revert NotApproved();
        // The recipient is this station's own address, which is the vault's
        // station on every chain, so a send can only reach the vault's own.
        _run(
            IBridge(implementation).send(address(this), toChain, address(this), amount, extra),
            IBridge(implementation).targets()
        );
        _sends.push(Send({toChain: toChain, amount: amount, sentAt: uint64(block.timestamp)}));
        emit Bridged(name, _sends.length, toChain, amount);
    }

    function _sendHome(uint256 amount) private {
        _asset().forceApprove(_spec.vault, amount);
        IStationVault(_spec.vault).fromStation(amount);
    }

    /// Make each call after checking where it goes: to one of the driver's
    /// targets, or to the dollar only to approve one of them.
    function _run(Call[] memory calls, address[] memory targets) private {
        address dollar = address(_asset());
        for (uint256 i = 0; i < calls.length; i++) {
            Call memory c = calls[i];
            if (c.target == dollar) {
                if (c.data.length != 68 || bytes4(c.data) != APPROVE) revert BadCall();
                (address spender,) = abi.decode(_args(c.data), (address, uint256));
                if (!_includes(targets, spender)) revert BadCall();
            } else if (!_includes(targets, c.target)) {
                revert BadCall();
            }
            (bool ok, bytes memory reason) = c.target.call{value: c.value}(c.data);
            if (!ok) {
                assembly {
                    revert(add(reason, 32), mload(reason))
                }
            }
        }
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

    function _unwindDriver(bytes32 driver) private view returns (address implementation) {
        implementation = _registry().unwindDriver(driver);
        if (implementation == address(0)) revert NotApproved();
    }

    function _registry() private view returns (IAttestationRegistry) {
        return IAttestationRegistry(factory.attestation());
    }

    function _asset() private view returns (IERC20) {
        return IERC20(factory.asset());
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
