// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Allowances} from "../../../../../contracts/evm/src/interfaces/IPrices.sol";
import {IStation, Stage} from "../../../../../contracts/evm/src/interfaces/IStation.sol";
import {
    ClaimState,
    Claim,
    Fees,
    IVault,
    Request,
    RequestKind,
    StationPost,
    Tank,
    Timers,
    Valuation,
    ValuationStatus,
    VaultInit,
    VaultState
} from "../../../../../contracts/evm/src/interfaces/IVault.sol";
import {Scripted} from "./Scripted.sol";

/// A vault as the keeper sees it, for the keeper's differential tests: every
/// view answers from the script, and each call the keeper makes has the
/// effect a vault's would have on those views, enough to drive every
/// decision the keeper takes. It holds no money and checks no signature.
///
/// Script keys (see `Scripted.key`), each a step list in chain time:
/// `state`, `haltedAt`, `timers`/i (i in `Timers` order), `tank.*`,
/// `unsettled`, `lastPayment`, `nextUnpriced`, `needsValuationFirst`,
/// `readyToClose`, `homeStation`, `author`, `bundleHash`, `createdAt`,
/// `retiredAt`, `cash`, `minimumDeposit`, `balanceOf`/owner,
/// `claimCount`, `claim.*`/id, `claim.outcome`/id (the state a try leaves,
/// zero to refuse it), `requestCount`, `request.*`/id, `units.total`,
/// `unitsOf`/holder, `unitsSince`/holder, `serve.outcome`/holder (0 pays,
/// 1 refuses `NotYet`, 2 refuses `Nothing`, 3 refuses `NotYet` until a
/// valuation at or after `serve.after`/holder is priced), `value.gross`,
/// `value.cash`, `closer`, `closer.allowed`, `restricted.count` (codes on
/// the restricted list), `restricted.wanted` (codes the drivers name).
contract StandInVault is Scripted, IVault {
    uint256 private constant DAY = 86_400;

    Valuation[] private valuations;

    // Setup ------------------------------------------------------------------

    /// Valuations recorded before the test began.
    function seedValuation(Valuation calldata valuation) external {
        if (msg.sender != scripter) revert NotTheScripter();
        valuations.push(valuation);
    }

    /// Logs from before the test began, which the keeper reads.
    function emitProRataTaken(address owner, uint256 shares, uint256 assets, uint256 units_) external {
        if (msg.sender != scripter) revert NotTheScripter();
        emit ProRataTaken(owner, shares, assets, units_);
    }

    function emitStateChanged(VaultState state_) external {
        if (msg.sender != scripter) revert NotTheScripter();
        emit StateChanged(state_);
    }

    // What the keeper sends --------------------------------------------------

    function recordValuation() external {
        VaultState now_ = state();
        if (
            now_ != VaultState.Open && now_ != VaultState.WindingDown && now_ != VaultState.Frozen
                && now_ != VaultState.Killed
        ) revert WrongState();
        Timers memory t = timers();
        uint64 ts = uint64(block.timestamp);
        if (ts % t.slotInterval > t.slotWindow) revert OutsideWindow();
        uint64 time = ts - ts % t.slotInterval;
        if (valuations.length > 0 && valuations[valuations.length - 1].time >= time) revert AlreadyValued();
        if (unsettled() || valuing()) revert Unsettled();
        IStation station = IStation(homeStation());
        bool void = station.outsideBand() || station.stage().stagedAt != 0;
        valuations.push(
            Valuation({
                time: time,
                status: void ? ValuationStatus.Void : ValuationStatus.Priced,
                gross: _get("value.gross"),
                grossExit: _get("value.gross"),
                supply: 1,
                fee: 0,
                feeExit: 0,
                cash: _get("value.cash"),
                postsMissing: 0
            })
        );
        emit Valued(valuations.length - 1, time, _get("value.gross"), _get("value.gross"), 1, 0);
        if (!void) _priced(time);
    }

    function closeValuation(uint64 time) external {
        if (valuations.length == 0) revert Nothing();
        Valuation storage last = valuations[valuations.length - 1];
        if (last.status != ValuationStatus.Open || last.time != time) revert WrongState();
        if (block.timestamp <= uint256(time) + timers().postDeadline) revert NotYet();
        last.status = ValuationStatus.Priced;
        _priced(time);
    }

    /// What a priced valuation at `time` does to the views.
    function _priced(uint64 time) private {
        if (time >= _nextMonday(lastPayment())) _set("lastPayment", time);
        uint256 waiting = _get("nextUnpriced");
        if (waiting != 0 && waiting <= time) {
            _set("nextUnpriced", 0);
            _set("unsettled", 1);
        }
        _set("needsValuationFirst", 0);
        (uint256 stationMark,) = IStation(homeStation()).value();
        if (haltedAt() != 0 && time > haltedAt() && stationMark == 0 && !unsettled()) {
            _set("readyToClose", 1);
        }
    }

    function settle(uint256) external returns (uint256) {
        if (!unsettled()) revert Nothing();
        _set("unsettled", 0);
        return 1;
    }

    function settleClaim(uint256 claimId) external {
        ClaimState was = ClaimState(_get("claim.state", claimId));
        if (was != ClaimState.Pending && was != ClaimState.Waiting) revert WrongState();
        if (_get("needsValuationFirst") != 0) revert NotYet();
        uint256 outcome = _get("claim.outcome", claimId);
        if (outcome == 0) revert NotYet();
        _set("claim.state", claimId, outcome);
        _set("claim.lastAttemptAt", claimId, block.timestamp);
        emit ClaimTried(claimId, ClaimState(outcome) == ClaimState.Waiting);
        if (ClaimState(outcome) == ClaimState.Paid) emit ClaimPaid(claimId, 0);
    }

    function freeze(uint256 claimId) external {
        ClaimState was = ClaimState(_get("claim.state", claimId));
        if (was == ClaimState.None || was == ClaimState.Paid || was == ClaimState.Fallback) revert WrongState();
        if (haltedAt() != 0) revert WrongState();
        if (block.timestamp < _get("claim.bookedAt", claimId) + timers().freezeAfter) revert NotYet();
        if (unsettled()) revert Unsettled();
        _halt(VaultState.Frozen, "claim");
    }

    function freezeUnits(address holder) external {
        if (_get("unitsOf", _word(holder)) == 0) revert Nothing();
        if (haltedAt() != 0) revert WrongState();
        if (block.timestamp < unitsSince(holder) + timers().freezeAfter) revert NotYet();
        if (unsettled()) revert Unsettled();
        _halt(VaultState.Frozen, "units");
    }

    function serveProRata(address holder) external {
        uint256 held = _get("unitsOf", _word(holder));
        if (held == 0) revert Nothing();
        uint256 outcome = _get("serve.outcome", _word(holder));
        if (outcome == 1) revert NotYet();
        if (outcome == 2) revert Nothing();
        if (outcome == 3) {
            (bool priced,, uint64 time) = lastPriced();
            if (!priced || time < _get("serve.after", _word(holder))) revert NotYet();
        }
        _set("unitsOf", _word(holder), 0);
        _set("unitsSince", _word(holder), 0);
        _set("units.total", _get("units.total") - held);
        emit UnitsPaid(holder, held);
    }

    function keep(uint256 total, uint64 asOf, bytes calldata) external {
        if (total == 0 && asOf == 0) {
            if (_get("tank.keepAt") <= _get("tank.balance") || _get("tank.fillPending") != 0 || haltedAt() != 0) {
                revert Nothing();
            }
            _set("tank.fillPending", 1);
            return;
        }
        if (asOf > block.timestamp) revert Stale();
        if (asOf <= _get("tank.usageAsOf") || total <= _get("tank.usageSettled")) revert Stale();
        if (_get("tank.balance") == 0) revert Nothing();
        uint256 before = _get("tank.usageSettled");
        _set("tank.usageSettled", total);
        _set("tank.usageAsOf", asOf);
        _set("tank.lastCollection", block.timestamp);
        emit TankCharged(total - before, total - before, total);
    }

    /// Read the drivers' lists again: the list grows to `restricted.wanted`
    /// codes when the script says the drivers name more than it holds.
    function refreshRestricted() external {
        if (state() != VaultState.Open) revert WrongState();
        uint256 wanted = _get("restricted.wanted");
        if (wanted > _get("restricted.count")) _set("restricted.count", wanted);
    }

    function cancel() external {
        if (state() != VaultState.Activating) revert WrongState();
        if (block.timestamp < createdAt() + timers().activationWindow) revert NotYet();
        _setState(VaultState.Cancelled);
    }

    function evict() external {
        if (state() != VaultState.WindingDown) revert WrongState();
        if (block.timestamp < _get("retiredAt") + timers().windDown) revert NotYet();
        _halt(VaultState.Killed, "evicted");
    }

    function close() external {
        VaultState now_ = state();
        if (haltedAt() == 0 || now_ == VaultState.Closed || now_ == VaultState.Cancelled) revert WrongState();
        if (!readyToClose() || unsettled()) revert NotYet();
        _setState(VaultState.Closed);
    }

    function settleClosed(uint256 requestId) external returns (uint256) {
        if (state() != VaultState.Closed) revert WrongState();
        if (_get("request.done", requestId) != 0) revert Nothing();
        if (RequestKind(_get("request.kind", requestId)) == RequestKind.Claim) revert WrongState();
        _set("request.done", requestId, 1);
        return 0;
    }

    function claimClosed(uint256 claimId) external returns (uint256) {
        if (state() != VaultState.Closed) revert WrongState();
        ClaimState was = ClaimState(_get("claim.state", claimId));
        if (was != ClaimState.Pending && was != ClaimState.Waiting && was != ClaimState.Queued) revert WrongState();
        _set("claim.state", claimId, uint256(ClaimState.Paid));
        return 0;
    }

    /// Whether the scripted Closer may close this vault now.
    function closerMayClose() public view returns (bool) {
        VaultState now_ = state();
        return haltedAt() != 0 && block.timestamp >= uint256(haltedAt()) + haltGrace()
            && _get("closer.allowed") != 0 && now_ != VaultState.Closed && now_ != VaultState.Cancelled;
    }

    /// The Closer's last step: the station stopped and emptied, the vault
    /// closes.
    function closeByCloser() external {
        if (msg.sender != _addr(_get("closer"))) revert NotAllowed();
        if (!closerMayClose()) revert NotYet();
        _set("readyToClose", 1);
        _setState(VaultState.Closed);
    }

    function _halt(VaultState to, bytes32 reason) private {
        _set("haltedAt", block.timestamp);
        emit Halted(reason, uint64(block.timestamp));
        _setState(to);
    }

    function _setState(VaultState to) private {
        _set("state", uint256(to));
        emit StateChanged(to);
    }

    function _nextMonday(uint64 t) private pure returns (uint64) {
        uint256 sinceMonday = t / DAY + 3;
        return uint64((sinceMonday - sinceMonday % 7 + 4) * DAY);
    }

    // What the keeper reads --------------------------------------------------

    function state() public view returns (VaultState) {
        return VaultState(_get("state"));
    }

    function timers() public view returns (Timers memory t) {
        t.slotInterval = uint64(_get("timers", 0));
        t.slotLead = uint64(_get("timers", 1));
        t.slotWindow = uint64(_get("timers", 2));
        t.postDeadline = uint64(_get("timers", 3));
        t.transitTimeout = uint64(_get("timers", 4));
        t.fallbackAfter = uint64(_get("timers", 5));
        t.usageFreshness = uint64(_get("timers", 6));
        t.tankEscapeDelay = uint64(_get("timers", 7));
        t.floorDelay = uint64(_get("timers", 8));
        t.haltGrace = uint64(_get("timers", 9));
        t.freezeAfter = uint64(_get("timers", 10));
        t.windDown = uint64(_get("timers", 11));
        t.activationWindow = uint64(_get("timers", 12));
    }

    function tank() external view returns (Tank memory t) {
        t.balance = _get("tank.balance");
        t.keepAt = _get("tank.keepAt");
        t.usageSettled = _get("tank.usageSettled");
        t.usageAsOf = uint64(_get("tank.usageAsOf"));
        t.lastCollection = uint64(_get("tank.lastCollection"));
        t.escapeRequestedAt = uint64(_get("tank.escapeRequestedAt"));
        t.capCarried = _get("tank.capCarried");
        t.fillPending = _get("tank.fillPending") != 0;
    }

    function haltedAt() public view returns (uint64) {
        return uint64(_get("haltedAt"));
    }

    function haltGrace() public view returns (uint64) {
        return uint64(_get("timers", 9));
    }

    function valuing() public view returns (bool) {
        return valuations.length > 0 && valuations[valuations.length - 1].status == ValuationStatus.Open;
    }

    function unsettled() public view returns (bool) {
        return _get("unsettled") != 0;
    }

    function lastPriced() public view returns (bool priced, uint256 index, uint64 time) {
        for (uint256 i = valuations.length; i > 0; i--) {
            if (valuations[i - 1].status == ValuationStatus.Priced) return (true, i - 1, valuations[i - 1].time);
        }
        return (false, 0, 0);
    }

    function lastPayment() public view returns (uint64) {
        return uint64(_get("lastPayment"));
    }

    function nextUnpriced() external view returns (uint64) {
        return uint64(_get("nextUnpriced"));
    }

    function needsValuationFirst() external view returns (bool) {
        return _get("needsValuationFirst") != 0;
    }

    function readyToClose() public view returns (bool) {
        return _get("readyToClose") != 0;
    }

    function closeWantsValuation(uint64 time) external view returns (bool) {
        VaultState now_ = state();
        if (haltedAt() == 0 || now_ == VaultState.Closed || time <= haltedAt() || readyToClose() || valuing()) {
            return false;
        }
        IStation station = IStation(homeStation());
        (uint256 stationMark,) = station.value();
        return stationMark == 0 && !station.outsideBand() && station.stage().stagedAt == 0;
    }

    function valuationCount() external view returns (uint256) {
        return valuations.length;
    }

    function valuationAt(uint256 index) external view returns (Valuation memory) {
        return valuations[index];
    }

    function claimCount() external view returns (uint256) {
        return _get("claimCount");
    }

    function claimAt(uint256 claimId) external view returns (Claim memory c) {
        c.owner = _addr(_get("claim.owner", claimId));
        c.shares = _get("claim.shares", claimId);
        c.bookedAt = uint64(_get("claim.bookedAt", claimId));
        c.lastAttemptAt = uint64(_get("claim.lastAttemptAt", claimId));
        c.state = ClaimState(_get("claim.state", claimId));
        c.paid = _get("claim.paid", claimId);
    }

    function requestCount() external view returns (uint256) {
        return _get("requestCount");
    }

    function requestAt(uint256 requestId) external view returns (Request memory r) {
        r.owner = _addr(_get("request.owner", requestId));
        r.kind = RequestKind(_get("request.kind", requestId));
        r.earliest = uint64(_get("request.earliest", requestId));
        r.done = _get("request.done", requestId) != 0;
        r.amount = _get("request.amount", requestId);
        r.ref = _get("request.ref", requestId);
    }

    function units() external view returns (uint256 total, uint256 reserve) {
        return (_get("units.total"), 0);
    }

    function unitsOf(address holder) external view returns (uint256 units_, uint256 claimable) {
        return (_get("unitsOf", _word(holder)), 0);
    }

    function unitsSince(address holder) public view returns (uint64) {
        return uint64(_get("unitsSince", _word(holder)));
    }

    function homeStation() public view returns (address) {
        return _addr(_get("homeStation"));
    }

    function author() external view returns (address) {
        return _addr(_get("author"));
    }

    function bundleHash() external view returns (bytes32) {
        return bytes32(_get("bundleHash"));
    }

    function createdAt() public view returns (uint64) {
        return uint64(_get("createdAt"));
    }

    function balanceOf(address owner) external view returns (uint256) {
        return _get("balanceOf", _word(owner));
    }

    function cash() external view returns (uint256) {
        return _get("cash");
    }

    function minimumDeposit() external view returns (uint256) {
        return _get("minimumDeposit");
    }

    function exitBandBps() external view returns (uint16) {
        return uint16(_get("exitBandBps"));
    }

    function strategyId() external view returns (uint256) {
        return _get("strategyId");
    }

    function totalSupply() external view returns (uint256) {
        return _get("totalSupply");
    }

    function mark() external view returns (uint256) {
        return _get("mark");
    }

    function escrow() external pure returns (uint256) {
        return 0;
    }

    function heldFor(address) external pure returns (uint256) {
        return 0;
    }

    function openedAt(uint256 index) external view returns (uint64) {
        return valuations[index].time;
    }

    function openedBlock(uint256) external pure returns (uint64) {
        return 0;
    }

    function fees() external pure returns (Fees memory) {}

    function allowances() external pure returns (Allowances memory) {}

    function idleFloor() external pure returns (uint16, uint16, uint64) {}

    function sharePrice() external pure returns (uint256) {
        return 1e18;
    }

    function totalValue() external pure returns (uint256) {
        return 0;
    }

    function exitValue() external pure returns (uint256) {
        return 0;
    }

    function name() external pure returns (string memory) {
        return "stand-in vault";
    }

    function symbol() external pure returns (string memory) {
        return "SIV";
    }

    function decimals() external pure returns (uint8) {
        return 6;
    }

    function restricted() external view returns (bytes32[] memory codes) {
        codes = new bytes32[](_get("restricted.count"));
        for (uint256 i = 0; i < codes.length; i++) {
            codes[i] = bytes32(i + 1);
        }
    }

    function restrictedText() external pure returns (string memory) {}

    function statementOf(address) external pure returns (bytes32) {}

    function statementDigest(address) external pure returns (bytes32) {}

    function usageDigest(uint256, uint64) external pure returns (bytes32) {}

    function postDigest(uint64, uint256, StationPost calldata) external pure returns (bytes32) {}

    function exitCostDigest(uint256, uint256, address) external pure returns (bytes32) {}

    function exitCostOf(uint256) external pure returns (uint256, address) {}

    function maxExitCost() external pure returns (uint256) {}

    // Nothing in the tests calls these -------------------------------------

    function initialize(VaultInit calldata, uint256, uint256) external pure {
        revert NotScripted();
    }

    function requestDeposit(uint256, address, bytes calldata) external pure returns (uint256) {
        revert NotScripted();
    }

    function requestRedeem(uint256, address) external pure returns (uint256) {
        revert NotScripted();
    }

    function postStation(uint64, uint256, StationPost calldata, bytes calldata) external pure {
        revert NotScripted();
    }

    function takeProRata(uint256) external pure {
        revert NotScripted();
    }

    function takeProRataClaim(uint256) external pure {
        revert NotScripted();
    }

    function claimInstead(uint256) external pure returns (uint256) {
        revert NotScripted();
    }

    function reportExitCost(uint256, uint256, address, bytes calldata) external pure {
        revert NotScripted();
    }

    function collectUnits() external pure returns (uint256) {
        revert NotScripted();
    }

    function redeemClosed(uint256, address) external pure returns (uint256) {
        revert NotScripted();
    }

    function collectHeld() external pure returns (uint256) {
        revert NotScripted();
    }

    function fundTank(uint256) external pure {
        revert NotScripted();
    }

    function withdrawTank(uint256, uint256, uint64, bytes calldata) external pure {
        revert NotScripted();
    }

    function requestTankWithdrawal() external pure {
        revert NotScripted();
    }

    function withdrawTankUnsigned(uint256) external pure {
        revert NotScripted();
    }

    function topUpFromShares(uint256) external pure {
        revert NotScripted();
    }

    function setTankKeep(uint256) external pure {
        revert NotScripted();
    }

    function setAllowances(Allowances calldata) external pure {
        revert NotScripted();
    }

    function recordStatement(address, bytes calldata) external pure {
        revert NotScripted();
    }

    function setIdleFloor(uint16) external pure {
        revert NotScripted();
    }

    function retire() external pure {
        revert NotScripted();
    }

    function kill() external pure {
        revert NotScripted();
    }

    function windDownSealed() external pure {
        revert NotScripted();
    }

    function confirmStations() external pure {
        revert NotScripted();
    }

    function toStation(uint256) external pure {
        revert NotScripted();
    }

    function fromStation(uint256) external pure {
        revert NotScripted();
    }

    function delivered(uint256, uint256, uint256, uint256) external pure {
        revert NotScripted();
    }

    function unstaged() external pure {
        revert NotScripted();
    }
}
