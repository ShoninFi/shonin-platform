// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {CloseBand} from "./drivers/CloseBand.sol";
import {IAttestationRegistry, ImageKind} from "./interfaces/IAttestationRegistry.sol";
import {Call, ExitPath, IDelayedDriver, IDriver} from "./interfaces/IDriver.sol";
import {IStation, IStationFactory, IStationVault, Send, Stage, StationSpec} from "./interfaces/IStation.sol";
import {StationCloses} from "./station/StationCloses.sol";
import {Flow, StationLayout, StationStorage} from "./station/StationStorage.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

import {ReentrancyGuardTransient} from "openzeppelin/utils/ReentrancyGuardTransient.sol";
import {ECDSA} from "openzeppelin/utils/cryptography/ECDSA.sol";
import {MessageHashUtils} from "openzeppelin/utils/cryptography/MessageHashUtils.sol";

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
///
/// A venue that acts on the station's calls only after the transaction (a
/// `Delayed` driver) can't be closed and checked in one transaction. There a
/// claim's close is staged for that claim (or one holder's units) and
/// settled in later blocks, in whole or in part, always within its windows
/// (see `StationCloses`, where every close runs). Nothing moves between the
/// station and its venues meanwhile but the close's own sweep, so the
/// station can tell what came home for it: every dollar that reaches it
/// outside its own calls is counted (`arrivals`).
contract Station is IStation, ReentrancyGuardTransient {
    using StationStorage for StationLayout;

    uint256 private constant WAD = 1e18;
    /// A stopped station's full unwind keeps the exit band this long after
    /// the stop, then the band doubles each time this passes again.
    uint64 public constant WIDEN_AFTER = StationStorage.WIDEN_AFTER;
    /// What a best-effort revoke may spend in all: far more than a venue's
    /// revoke costs, far less than a block.
    uint256 private constant BEST_EFFORT_GAS = 2_000_000;
    /// The most calls a driver may prepare to revoke a key.
    uint256 private constant MAX_REVOKE_CALLS = 8;
    /// The most venue vaults one driver may have reclaimed from.
    uint256 private constant MAX_HELD = 8;

    bool private transient _inCall;

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
    error NotYet();
    error NothingHeld();

    /// The implementation every station is a clone of is never a station
    /// itself.
    constructor() {
        StationStorage.layout().factory = IStationFactory(address(1));
    }

    function initialize(StationSpec calldata spec_) external {
        StationLayout storage s = StationStorage.layout();
        if (address(s.factory) != address(0)) revert AlreadyInitialized();
        s.factory = IStationFactory(msg.sender);
        // Each driver and bridge once: a name listed twice would count its
        // venue twice and close a fraction of it twice.
        _unique(spec_.drivers);
        _unique(spec_.bridges);
        s.spec = spec_;
        // Pin what the registry approves now, for good.
        IAttestationRegistry registry = s.registry();
        uint256 delayed;
        for (uint256 i = 0; i < spec_.drivers.length; i++) {
            address implementation = registry.tradingDriver(spec_.drivers[i]);
            if (implementation == address(0)) revert NotApproved();
            s.pinnedDriver[spec_.drivers[i]] = implementation;
            if (IDriver(implementation).exitPath() == ExitPath.Delayed) delayed++;
        }
        // A station closes through drivers that all settle in the
        // transaction, or all after it: a stage beside a venue that closes at
        // once would sell there for a leaver whatever part of the delayed
        // close then happens, and a try that closes nothing would leave that
        // sale's spread with the holders who stay.
        if (delayed != 0 && delayed != spec_.drivers.length) revert MixedExits();
        for (uint256 i = 0; i < spec_.bridges.length; i++) {
            address implementation = registry.bridgeOf(spec_.bridges[i]);
            if (implementation == address(0)) revert NotApproved();
            s.pinnedBridge[spec_.bridges[i]] = implementation;
        }
    }

    // Keys ------------------------------------------------------------------

    function approveKey(bytes32 driver, address key) external nonReentrant {
        StationLayout storage s = StationStorage.layout();
        if (s.stopped) revert AlreadyStopped();
        if (!StationStorage.contains(s.spec.drivers, driver)) revert NotDeclared();
        if (s.reclaimed[driver]) revert NotAllowed();
        if (!_isKey(key, ImageKind.Executor)) revert NotBlessed();
        address implementation = _tradable(driver);

        // One executor at a time: the key in place is withdrawn first, so an
        // instance that has been replaced can no longer trade.
        address current = s.approvedKey[driver];
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
        uint64 until = s.registry().blessedUntil(key, s.spec.vaultChain, s.spec.vault, s.spec.bundleHash);
        s.run(
            IDriver(implementation).approveKey(address(this), key, until), IDriver(implementation).targets()
        );
        s.approvedKey[driver] = key;
        emit KeyApproved(driver, key, until);
    }

    function revokeKey(bytes32 driver, address key) external nonReentrant {
        StationLayout storage s = StationStorage.layout();
        // A passer-by can't cut off a key whose blessing stands, and can't
        // keep one whose blessing has gone.
        // Once the driver no longer trades, its key goes too.
        address implementation = s.pinned(driver);
        if (
            !s.stopped && !s.reclaimed[driver] && _isKey(key, ImageKind.Executor)
                && !s.registry().isDriverRevoked(driver, implementation)
        ) {
            revert StillBlessed();
        }
        _revokeStrict(driver, key, false);
        if (s.approvedKey[driver] == key) delete s.approvedKey[driver];
        emit KeyRevoked(driver, key);
    }

    // Moving money ----------------------------------------------------------

    function pull(uint256 amount) external nonReentrant tracked {
        _executorOnly();
        if (!isHome()) revert NotHome();
        IStationVault(StationStorage.layout().spec.vault).toStation(amount);
        emit Pulled(amount);
    }

    function push(uint256 amount) external nonReentrant tracked {
        _executorOnly();
        if (!isHome()) revert NotHome();
        StationStorage.layout().sendHome(amount);
        emit Pushed(amount);
    }

    function deposit(bytes32 driver, uint256 amount) external nonReentrant tracked {
        StationLayout storage s = StationStorage.layout();
        _executorOnly();
        address implementation = _tradable(driver);
        uint256 before = s.balance();
        s.run(IDriver(implementation).deposit(address(this), amount), IDriver(implementation).targets());
        // What left for the venue, counted for good: a venue valued from a
        // reading made elsewhere counts what was sent after the reading.
        uint256 left = s.balance();
        Flow storage flow = s.flows[driver];
        if (left < before) flow.sent += before - left;
        flow.depositedAt = uint64(block.timestamp);
        flow.movedAt = uint64(block.timestamp);
        emit Deposited(driver, amount);
    }

    function withdraw(bytes32 driver, uint256 amount) external nonReentrant tracked {
        StationLayout storage s = StationStorage.layout();
        _executorOnly();
        // Bringing money out never depends on the driver still being
        // approved to trade.
        address implementation = s.pinned(driver);
        s.run(IDriver(implementation).withdraw(address(this), amount), IDriver(implementation).targets());
        s.flows[driver].movedAt = uint64(block.timestamp);
        emit Withdrawn(driver, amount);
    }

    function bridge(
        bytes32 name,
        uint256 toChain,
        uint256 amount,
        bytes calldata extra
    ) external nonReentrant tracked {
        StationLayout storage s = StationStorage.layout();
        _executorOnly();
        if (toChain == block.chainid) revert NoRoute();
        // Nothing leaves the home station while a valuation waits for the
        // other chains' posts: it was counted here at the open, and landing
        // there before their posts it would be counted twice.
        if (isHome() && IStationVault(s.spec.vault).valuing()) revert NotAllowed();
        s.send(name, toChain, amount, extra);
    }

    // Stopping and unwinding ------------------------------------------------

    function stop() external nonReentrant {
        if (!isHome()) revert NotHome();
        address vault = StationStorage.layout().spec.vault;
        uint64 haltedAt = IStationVault(vault).haltedAt();
        if (haltedAt == 0 || block.timestamp < haltedAt + IStationVault(vault).haltGrace()) {
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

    function unwind(uint256 fraction)
        external
        nonReentrant
        tracked
        returns (uint256 sentHome, bool blocked)
    {
        StationLayout storage s = StationStorage.layout();
        if (fraction == 0 || fraction > WAD) revert NotAllowed();
        // Once stopped, a fraction is closed only for the vault's own claims
        // and serves, which wait for settlement themselves; the rest is a
        // full unwind, which the stopped unwind holds until settlement too.
        bool permitted = (isHome() && msg.sender == s.spec.vault)
            || (!s.stopped && (_isKey(msg.sender, ImageKind.Executor) || _isKey(msg.sender, ImageKind.Unwinder)))
            || (s.stopped && fraction == WAD);
        if (!permitted) revert NotAllowed();
        if (s.stopped && fraction == WAD) return StationCloses.unwindStopped(s.spec.drivers);
        (sentHome, blocked,) = StationCloses.close(fraction, false);
    }

    function stageClose(uint256 fraction)
        external
        nonReentrant
        tracked
        returns (uint256 sentHome, bool blocked, bool staged)
    {
        if (fraction == 0 || fraction > WAD || !isHome() || msg.sender != StationStorage.layout().spec.vault)
        {
            revert NotAllowed();
        }
        return StationCloses.close(fraction, true);
    }

    /// A stopped station's full unwind through the drivers named, then
    /// everything idle home; with none named, only the idle dollars. Anyone
    /// may call it, so a driver that burns its gas can be left out and can't
    /// keep the other venues' money, or the idle dollars, from coming home.
    function unwindStopped(bytes32[] calldata drivers)
        external
        nonReentrant
        tracked
        returns (uint256 sentHome, bool blocked)
    {
        if (!StationStorage.layout().stopped) revert NotHalted();
        return StationCloses.unwindStopped(drivers);
    }

    /// One driver's part of a stopped station's full unwind. Only the
    /// station calls it, from its unwind, so one driver's failure undoes only
    /// its own part.
    function closeAll(bytes32 name, uint16 band) external returns (bool outside) {
        if (msg.sender != address(this)) revert NotAllowed();
        return StationCloses.closeAll(name, band);
    }

    // Delayed venues --------------------------------------------------------

    function settleClose() external nonReentrant tracked {
        StationCloses.settle();
    }

    function stage() external view returns (Stage memory) {
        return StationStorage.layout().stage;
    }

    function arrivals() public view returns (uint256) {
        StationLayout storage s = StationStorage.layout();
        // Inside its own call, only what arrived before it began.
        if (_inCall) return s.arrived;
        uint256 held = s.balance();
        return held > s.seen ? s.arrived + held - s.seen : s.arrived;
    }

    function venueFlows(bytes32 driver) external view returns (uint256 sent, uint64 depositedAt) {
        Flow storage flow = StationStorage.layout().flows[driver];
        return (flow.sent, flow.depositedAt);
    }

    function movedAt(bytes32 driver) external view returns (uint64) {
        return StationStorage.layout().flows[driver].movedAt;
    }

    function fullUnwindAt(bytes32 driver) external view returns (uint64) {
        return StationStorage.layout().flows[driver].fullUnwindAt;
    }

    /// Money the station's account put into one of the venue's own vaults
    /// (which only its trading key could have done): count it from now on,
    /// bring it back as soon as the vault's lock allows, and end trading
    /// through that driver, withdrawing its key. Anyone may call it, and
    /// again to bring the money back once the lock ends; not while a close is
    /// staged, whose measure it would change.
    function reclaim(bytes32 driver, address venueVault) external nonReentrant tracked {
        StationLayout storage s = StationStorage.layout();
        if (s.stage.stagedAt != 0) revert NotYet();
        address implementation = s.pinned(driver);
        (Call[] memory calls, uint256 equity) =
            IDelayedDriver(implementation).reclaim(address(this), venueVault);
        address[] storage held = s.held[driver];
        bool listed;
        for (uint256 i = 0; i < held.length; i++) {
            if (held[i] == venueVault) listed = true;
        }
        if (!listed) {
            if (equity == 0) revert NothingHeld();
            if (held.length >= MAX_HELD) revert NotAllowed();
            held.push(venueVault);
        }
        s.run(calls, IDriver(implementation).targets());
        if (!s.reclaimed[driver]) {
            s.reclaimed[driver] = true;
            address key = s.approvedKey[driver];
            if (key != address(0)) {
                if (_revokeBestEffort(driver, key)) {
                    delete s.approvedKey[driver];
                    emit KeyRevoked(driver, key);
                } else {
                    emit RevokeSkipped(driver, key);
                }
            }
        }
        emit Reclaimed(driver, venueVault, equity);
    }

    function heldIn(bytes32 driver) external view returns (address[] memory) {
        return StationStorage.layout().held[driver];
    }

    function reclaimed(bytes32 driver) external view returns (bool) {
        return StationStorage.layout().reclaimed[driver];
    }

    // What it holds ---------------------------------------------------------

    function value() external view returns (uint256 mark, uint256 exit) {
        StationLayout storage s = StationStorage.layout();
        uint256 idle = s.balance();
        mark = idle;
        exit = idle;
        for (uint256 i = 0; i < s.spec.drivers.length; i++) {
            // Valued through the driver it can unwind through, so a revoked
            // driver's holdings still count.
            address implementation = s.pinned(s.spec.drivers[i]);
            (uint256 m, uint256 x) = IDriver(implementation).valueOf(address(this));
            mark += m;
            exit += x;
            address[] storage held = s.held[s.spec.drivers[i]];
            if (held.length != 0) {
                uint256 parked = IDelayedDriver(implementation).heldValue(address(this), held);
                mark += parked;
                exit += parked;
            }
        }
    }

    function outsideBand() external view returns (bool) {
        StationLayout storage s = StationStorage.layout();
        for (uint256 i = 0; i < s.spec.drivers.length; i++) {
            IDriver driver = IDriver(s.pinned(s.spec.drivers[i]));
            (, bool blocked) = driver.close(address(this), WAD, s.spec.exitBandBps);
            if (blocked) return true;
            // A delayed venue's value isn't trusted while dollars moved to or
            // from it may be in neither place, or while it is known only
            // from a stale reading: it prices nothing then.
            if (
                driver.exitPath() == ExitPath.Delayed
                    && !IDelayedDriver(address(driver)).priced(address(this))
            ) {
                return true;
            }
        }
        return false;
    }

    function factory() external view returns (IStationFactory) {
        return StationStorage.layout().factory;
    }

    function stopped() external view returns (bool) {
        return StationStorage.layout().stopped;
    }

    function stoppedAt() external view returns (uint64) {
        return StationStorage.layout().stoppedAt;
    }

    function spec() external view returns (StationSpec memory) {
        return StationStorage.layout().spec;
    }

    function isHome() public view returns (bool) {
        return StationStorage.layout().isHome();
    }

    function pinnedDriver(bytes32 driver) external view returns (address) {
        return StationStorage.layout().pinnedDriver[driver];
    }

    function pinnedBridge(bytes32 name) external view returns (address) {
        return StationStorage.layout().pinnedBridge[name];
    }

    /// The band a stopped station's full unwind closes inside: the exit band
    /// for `WIDEN_AFTER` after the stop, then doubling each `WIDEN_AFTER`
    /// until any price is accepted. No key sets it; only time does.
    function stoppedBand() public view returns (uint16) {
        StationLayout storage s = StationStorage.layout();
        if (!s.stopped) return s.spec.exitBandBps;
        return CloseBand.band(s.unwindBand());
    }

    function approvedKey(bytes32 driver) external view returns (address) {
        return StationStorage.layout().approvedKey[driver];
    }

    function sends() external view returns (Send[] memory) {
        return StationStorage.layout().sends;
    }

    function sendCount() external view returns (uint256) {
        return StationStorage.layout().sends.length;
    }

    function sendAt(uint256 index) external view returns (Send memory) {
        return StationStorage.layout().sends[index];
    }

    function stopDigest(bytes32 reason, uint64 haltedAt) public view returns (bytes32) {
        return keccak256(abi.encode(block.chainid, address(this), "shonin-stop", reason, haltedAt));
    }

    // Internals -------------------------------------------------------------

    function _stop(bytes32 reason, uint64 haltedAt) private {
        StationLayout storage s = StationStorage.layout();
        if (s.stopped) revert AlreadyStopped();
        s.stopped = true;
        s.stoppedAt = uint64(block.timestamp);
        for (uint256 i = 0; i < s.spec.drivers.length; i++) {
            bytes32 driver = s.spec.drivers[i];
            address key = s.approvedKey[driver];
            if (key == address(0)) continue;
            // A venue or a driver that refuses the revoke can't keep the
            // station from stopping. A key whose revoke didn't go through
            // stays recorded, and anyone may withdraw it with `revokeKey`.
            if (_revokeBestEffort(driver, key)) {
                delete s.approvedKey[driver];
                emit KeyRevoked(driver, key);
            } else {
                emit RevokeSkipped(driver, key);
            }
        }
        emit Stopped(reason, haltedAt);
    }

    /// Withdraw `key` at the venue through the pinned driver; every call
    /// must go through.
    function _revokeStrict(bytes32 driver, address key, bool capped) private {
        StationLayout storage s = StationStorage.layout();
        IDriver implementation = IDriver(s.pinned(driver));
        Call[] memory calls = implementation.revokeKey(address(this), key);
        if (capped && calls.length > MAX_REVOKE_CALLS) revert BadCall();
        s.run(calls, implementation.targets());
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

    /// The executor's own moves; none while a close is staged, so nothing
    /// moves between the station and its venues but the close's sweep.
    function _executorOnly() private view {
        StationLayout storage s = StationStorage.layout();
        if (s.stopped) revert AlreadyStopped();
        if (s.stage.stagedAt != 0) revert NotYet();
        if (!_isKey(msg.sender, ImageKind.Executor)) revert NotBlessed();
    }

    /// A key blessed, on this chain, for this vault and its bundle, from an
    /// image of that kind.
    function _isKey(address key, ImageKind kind) private view returns (bool) {
        StationLayout storage s = StationStorage.layout();
        IAttestationRegistry registry = s.registry();
        return registry.isBlessedFor(key, s.spec.vaultChain, s.spec.vault, s.spec.bundleHash)
            && registry.keyInfo(key).kind == kind;
    }

    /// The pinned implementation, if it may still trade.
    function _tradable(bytes32 driver) private view returns (address implementation) {
        StationLayout storage s = StationStorage.layout();
        implementation = s.pinned(driver);
        if (s.registry().isDriverRevoked(driver, implementation) || s.reclaimed[driver]) revert NotApproved();
    }

    /// Count what arrived since the last call before this one moves
    /// anything, and note the balance it leaves. Every call that can move
    /// the station's dollars is tracked; a key's approval or revoke and a
    /// stop move none.
    modifier tracked() {
        _enter();
        _;
        _leave();
    }

    function _enter() private {
        StationStorage.layout().arrived = arrivals();
        _inCall = true;
    }

    function _leave() private {
        StationLayout storage s = StationStorage.layout();
        s.seen = s.balance();
        _inCall = false;
    }

    function _unique(bytes32[] calldata list) private pure {
        for (uint256 i = 0; i < list.length; i++) {
            for (uint256 j = i + 1; j < list.length; j++) {
                if (list[i] == list[j]) revert NotAllowed();
            }
        }
    }
}
