// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationRegistry, ImageKind} from "../interfaces/IAttestationRegistry.sol";
import {IDriver} from "../interfaces/IDriver.sol";
import {Allowances, IPrices} from "../interfaces/IPrices.sol";
import {IStation} from "../interfaces/IStation.sol";
import {
    Claim,
    ClaimState,
    IVault,
    Request,
    RequestKind,
    Valuation,
    ValuationStatus,
    VaultInit,
    VaultState
} from "../interfaces/IVault.sol";
import {Codes} from "./Codes.sol";
import {Valuations} from "./Valuations.sol";
import {Layout, VaultStorage} from "./VaultStorage.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";
import {ECDSA} from "openzeppelin/utils/cryptography/ECDSA.sol";
import {MessageHashUtils} from "openzeppelin/utils/cryptography/MessageHashUtils.sol";
import {SignatureChecker} from "openzeppelin/utils/cryptography/SignatureChecker.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// Everything about a vault that isn't pricing: setting it up, who may
/// deposit, the tank, claims, its lifecycle and its idle floor.
library Upkeep {
    using VaultStorage for Layout;
    using Math for uint256;
    using SafeERC20 for IERC20;

    uint256 private constant OFFSET = VaultStorage.OFFSET;
    uint256 private constant BPS = VaultStorage.BPS;
    uint256 private constant WAD = VaultStorage.WAD;
    /// What a driver may spend, and how many codes it may name, when its
    /// restricted list is read again.
    uint256 private constant RESTRICTED_GAS = 200_000;
    uint256 private constant MAX_RESTRICTED = 64;

    string internal constant STATEMENT =
        "I am not a restricted person for this vault, and I am not acting for one.";
    bytes32 private constant DOMAIN_TYPE =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
    bytes32 private constant STATEMENT_TYPE =
        keccak256("Statement(address depositor,string statement,string restricted)");

    // Setting up ------------------------------------------------------------

    /// The tank has already been moved in. The dead shares come out of it:
    /// a dollar's cent that nobody can ever redeem, so the first depositor
    /// can't be rounded down by a donation.
    function initialize(VaultInit calldata init, uint256 tankFunding, uint256 deadAssets) external {
        Layout storage s = VaultStorage.layout();
        if (s.factory != address(0)) revert IVault.NotAllowed();
        if (deadAssets == 0 || tankFunding <= deadAssets) revert IVault.TooSmall();
        s.factory = msg.sender;
        s.asset = init.asset;
        s.attestation = init.attestation;
        s.prices = init.prices;
        s.homeStation = init.homeStation;
        s.platform = init.platform;
        s.admin = init.admin;
        s.author = init.author;
        s.strategyId = init.strategyId;
        s.bundleHash = init.bundleHash;
        s.createdAt = uint64(block.timestamp);
        s.exitBandBps = init.exitBandBps;
        s.fees = init.fees;
        s.timers = init.timers;
        s.minimumDeposit = init.minimumDeposit;
        s.otherChains = init.otherChains;
        s.maxExitCost = init.maxExitCost;
        s.drivers = IStation(init.homeStation).spec().drivers;
        s.restricted = init.restricted;
        s.restrictedHash = keccak256(abi.encode(init.restricted));
        s.floorBps = init.idleFloorBps;
        s.allowances = init.allowances;
        s.allowancesSince = uint64(block.timestamp);
        s.tank.keepAt = init.tankKeep;
        s.tank.lastCollection = uint64(block.timestamp);
        s.mark = WAD;
        s.lastHolding = uint64(block.timestamp);
        s.lastPayment = uint64(block.timestamp);

        s.tank.balance = tankFunding - deadAssets;
        s.cash = deadAssets;
        s.mint(VaultStorage.DEAD, deadAssets * OFFSET);
        emit IVault.TankFunded(init.author, tankFunding - deadAssets);

        s.state = init.otherChains.length == 0 ? VaultState.Open : VaultState.Activating;
        emit IVault.StateChanged(s.state);
    }

    // Who may deposit -------------------------------------------------------

    function statementDigest(address depositor) public view returns (bytes32) {
        bytes32 domain = keccak256(
            abi.encode(DOMAIN_TYPE, keccak256("Shonin Vault"), keccak256("1"), block.chainid, address(this))
        );
        bytes32 statement = keccak256(
            abi.encode(
                STATEMENT_TYPE, depositor, keccak256(bytes(STATEMENT)), keccak256(bytes(restrictedText()))
            )
        );
        return MessageHashUtils.toTypedDataHash(domain, statement);
    }

    /// A person's signed statement for the list as it stands. Contract
    /// wallets sign through ERC-1271.
    function recordStatement(address depositor, bytes calldata signature) external {
        Layout storage s = VaultStorage.layout();
        if (!SignatureChecker.isValidSignatureNow(depositor, statementDigest(depositor), signature)) {
            revert IVault.BadSignature();
        }
        s.statementOf[depositor] = s.restrictedHash;
        emit IVault.StatementRecorded(depositor, s.restrictedHash);
    }

    /// Read the same-chain drivers' lists again, from the implementation the
    /// registry approves now: a venue's terms can exclude more people after a
    /// vault was created, and its pinned driver can't learn that. The list
    /// only ever grows, and a depositor signs again for the longer one. Only
    /// a list is read, never a call made, and a driver that fails or answers
    /// with an outsized list is skipped.
    function refreshRestricted() external {
        Layout storage s = VaultStorage.layout();
        IAttestationRegistry registry = IAttestationRegistry(s.attestation);
        bool added;
        for (uint256 i = 0; i < s.drivers.length; i++) {
            address implementation = registry.tradingDriver(s.drivers[i]);
            if (implementation == address(0)) continue;
            bytes32[] memory codes;
            try IDriver(implementation).restricted{gas: RESTRICTED_GAS}() returns (bytes32[] memory list) {
                codes = list;
            } catch {
                continue;
            }
            if (codes.length > MAX_RESTRICTED) continue;
            for (uint256 j = 0; j < codes.length; j++) {
                if (_insert(s.restricted, codes[j])) {
                    added = true;
                    emit IVault.RestrictedAdded(codes[j]);
                }
            }
        }
        if (added) s.restrictedHash = keccak256(abi.encode(s.restricted));
    }

    /// Keep the list sorted and without repeats.
    function _insert(bytes32[] storage list, bytes32 code) private returns (bool) {
        uint256 at = list.length;
        for (uint256 i = 0; i < list.length; i++) {
            if (list[i] == code) return false;
            if (list[i] > code) {
                at = i;
                break;
            }
        }
        list.push(code);
        for (uint256 i = list.length - 1; i > at; i--) {
            list[i] = list[i - 1];
        }
        list[at] = code;
        return true;
    }

    /// The list as the statement carries it: codes, comma-separated.
    function restrictedText() public view returns (string memory) {
        return Codes.text(VaultStorage.layout().restricted);
    }

    // The tank --------------------------------------------------------------

    function fundTank(uint256 assets) external {
        Layout storage s = VaultStorage.layout();
        IERC20(s.asset).safeTransferFrom(msg.sender, address(this), assets);
        s.tank.balance += assets;
        emit IVault.TankFunded(msg.sender, assets);
    }

    function usageDigest(uint256 total, uint64 asOf) public view returns (bytes32) {
        return keccak256(abi.encode(block.chainid, address(this), "shonin-usage", total, asOf));
    }

    /// Collect the bill, then, if the tank is below what the author asked it
    /// kept at, queue a fill from their shares. Anyone can send it, every day.
    function keep(uint256 total, uint64 asOf, bytes calldata signature) external {
        Layout storage s = VaultStorage.layout();
        if (signature.length != 0) _collect(s, total, asOf, signature);
        VaultState state = s.state;
        if (
            s.tank.keepAt > s.tank.balance && !s.tank.fillPending && state != VaultState.Activating
                && state != VaultState.Closed && state != VaultState.Cancelled && !s.readyToClose()
        ) {
            s.tank.fillPending = true;
            s.queue(s.author, RequestKind.TankFill, s.tank.keepAt - s.tank.balance, 1);
        }
    }

    /// Charge the running total signed by the vault's executor, believing it
    /// only up to the author's allowances at published prices for the time it
    /// covers. What isn't charged is written off: the tank never owes.
    function _collect(Layout storage s, uint256 total, uint64 asOf, bytes calldata signature) private {
        bytes32 digest = MessageHashUtils.toEthSignedMessageHash(usageDigest(total, asOf));
        (address signer, ECDSA.RecoverError err,) = ECDSA.tryRecover(digest, signature);
        IAttestationRegistry registry = IAttestationRegistry(s.attestation);
        if (
            err != ECDSA.RecoverError.NoError
                || !registry.isBlessedFor(signer, block.chainid, address(this), s.bundleHash)
                || registry.keyInfo(signer).kind != ImageKind.Executor
        ) revert IVault.NotBlessed();
        // Each total is signed later than the last, so an old statement can't be
        // replayed to reset the cap that is carried forward.
        if (total < s.tank.usageSettled || asOf <= s.tank.usageAsOf || asOf > block.timestamp) {
            revert IVault.Stale();
        }

        uint64 end = _capEnd(s, asOf);
        uint64 start = s.tank.lastCollection;
        if (end < start) end = start;
        uint256 cap = s.tank.capCarried + _cost(s, start, end);
        uint256 billed = total - s.tank.usageSettled;
        uint256 charge = billed < cap ? billed : cap;
        if (charge > s.tank.balance) charge = s.tank.balance;

        s.tank.usageSettled = total;
        s.tank.usageAsOf = asOf;
        s.tank.lastCollection = end;
        s.tank.capCarried = 0;
        s.tank.balance -= charge;
        IERC20(s.asset).safeTransfer(s.platform, charge);
        emit IVault.TankCharged(billed, charge, total);
    }

    /// The cap stops when the strategy does: at the halt, plus the grace it
    /// is given to wind down.
    function _capEnd(Layout storage s, uint64 at) private view returns (uint64) {
        if (s.haltedAt == 0) return at;
        uint64 stop = s.haltedAt + s.timers.haltGrace;
        return at > stop ? stop : at;
    }

    /// What the allowances in force cost from `start`, or from when they
    /// were set if later, to `end`.
    function _cost(Layout storage s, uint64 start, uint64 end) private view returns (uint256) {
        uint64 from = s.allowancesSince > start ? s.allowancesSince : start;
        if (end <= from) return 0;
        return IPrices(s.prices).costOf(s.allowances, from, end);
    }

    function withdrawTank(uint256 assets, uint256 total, uint64 asOf, bytes calldata signature) external {
        Layout storage s = VaultStorage.layout();
        _authorOnly(s);
        // The bill is settled first, against a total recent enough to cover
        // what the agent has used.
        if (uint256(asOf) + s.timers.usageFreshness < block.timestamp) revert IVault.Stale();
        // A bill somebody has already collected needn't be collected again:
        // the author's withdrawal goes ahead on it.
        if (total != s.tank.usageSettled || asOf != s.tank.usageAsOf) _collect(s, total, asOf, signature);
        _payTank(s, assets);
    }

    /// With no executor running nobody can sign a total. After a delay long
    /// enough for anyone to collect a final bill, the author can withdraw
    /// without one, within the same length of time again: a request lapses,
    /// so it can't be armed early and used once an executor runs again.
    function requestTankWithdrawal() external {
        Layout storage s = VaultStorage.layout();
        _authorOnly(s);
        s.tank.escapeRequestedAt = uint64(block.timestamp);
        emit IVault.TankWithdrawalRequested(uint64(block.timestamp) + s.timers.tankEscapeDelay);
    }

    function withdrawTankUnsigned(uint256 assets) external {
        Layout storage s = VaultStorage.layout();
        _authorOnly(s);
        if (s.state != VaultState.Closed && s.state != VaultState.Cancelled) {
            uint64 asked = s.tank.escapeRequestedAt;
            uint256 delay = s.timers.tankEscapeDelay;
            if (asked == 0 || block.timestamp < asked + delay) revert IVault.NotYet();
            if (block.timestamp > asked + 2 * delay) revert IVault.NotAllowed();
            s.tank.escapeRequestedAt = 0;
        }
        _payTank(s, assets);
    }

    function _payTank(Layout storage s, uint256 assets) private {
        if (assets > s.tank.balance) assets = s.tank.balance;
        s.tank.balance -= assets;
        IERC20(s.asset).safeTransfer(s.author, assets);
        emit IVault.TankWithdrawn(assets);
    }

    function topUpFromShares(uint256 shares) external {
        Layout storage s = VaultStorage.layout();
        _authorOnly(s);
        VaultState state = s.state;
        if (
            state == VaultState.Activating || state == VaultState.Closed || state == VaultState.Cancelled
                || s.readyToClose()
        ) {
            revert IVault.WrongState();
        }
        if (shares == 0) revert IVault.TooSmall();
        s.queue(s.author, RequestKind.TankFill, shares, 0);
    }

    function setTankKeep(uint256 assets) external {
        Layout storage s = VaultStorage.layout();
        _authorOnly(s);
        s.tank.keepAt = assets;
        emit IVault.TankKeepSet(assets);
    }

    /// Close the cap's interval at the old allowances, then run on the new.
    function setAllowances(Allowances calldata allowances) external {
        Layout storage s = VaultStorage.layout();
        _authorOnly(s);
        uint64 end = _capEnd(s, uint64(block.timestamp));
        s.tank.capCarried += _cost(s, s.tank.lastCollection, end);
        s.allowances = allowances;
        s.allowancesSince = uint64(block.timestamp);
        emit IVault.AllowancesSet(allowances);
    }

    // Claims ----------------------------------------------------------------

    /// A claim nobody has tried to serve for `freezeAfter` freezes the vault.
    function freeze(uint256 claimId) external {
        Layout storage s = VaultStorage.layout();
        Claim storage claim = s.claims[claimId - 1];
        ClaimState state = claim.state;
        if (state != ClaimState.Pending && state != ClaimState.Waiting && state != ClaimState.Queued) {
            revert IVault.NotAllowed();
        }
        // Counted from when it was booked, whatever was tried since: a leaver
        // left unpaid this long is reason enough to stop, whether the market
        // or anyone else kept them waiting.
        if (block.timestamp < claim.bookedAt + s.timers.freezeAfter) revert IVault.NotYet();
        // A vault already halted (an evicted retirement) is stopping anyway.
        if (!s.live() || s.halted()) revert IVault.WrongState();
        _halt(s, "frozen");
        _setState(s, VaultState.Frozen);
    }

    /// Units nobody could serve for as long as an unpaid claim waits freeze
    /// the vault the same way: a close that can't come home must not keep a
    /// unit holder out for good either. Only where anyone may serve them; on
    /// a vault that trades on other chains only their holder can.
    function freezeUnits(address holder) external {
        Layout storage s = VaultStorage.layout();
        uint64 since = s.unitsSince[holder];
        if (s.unitsOf[holder] == 0 || s.otherChains.length != 0) revert IVault.NotAllowed();
        if (block.timestamp < since + s.timers.freezeAfter) revert IVault.NotYet();
        if (!s.live() || s.halted()) revert IVault.WrongState();
        _halt(s, "frozen");
        _setState(s, VaultState.Frozen);
    }

    // Lifecycle -------------------------------------------------------------

    /// A key blessed as this vault's executor says every station on another
    /// chain exists.
    function confirmStations() external {
        Layout storage s = VaultStorage.layout();
        if (s.state != VaultState.Activating) revert IVault.WrongState();
        IAttestationRegistry registry = IAttestationRegistry(s.attestation);
        if (
            !registry.isBlessedFor(msg.sender, block.chainid, address(this), s.bundleHash)
                || registry.keyInfo(msg.sender).kind != ImageKind.Executor
        ) revert IVault.NotBlessed();
        _setState(s, VaultState.Open);
    }

    /// A vault that never opened gives its author's money back.
    function cancel() external {
        Layout storage s = VaultStorage.layout();
        if (s.state != VaultState.Activating) revert IVault.WrongState();
        if (block.timestamp < s.createdAt + s.timers.activationWindow) revert IVault.NotYet();
        uint256 back = s.tank.balance + s.cash;
        s.tank.balance = 0;
        s.cash = 0;
        s.burn(VaultStorage.DEAD, s.balanceOf[VaultStorage.DEAD]);
        _setState(s, VaultState.Cancelled);
        IERC20(s.asset).safeTransfer(s.author, back);
    }

    function retire() external {
        Layout storage s = VaultStorage.layout();
        _authorOnly(s);
        if (s.state != VaultState.Open) revert IVault.WrongState();
        s.retiredAt = uint64(block.timestamp);
        _setState(s, VaultState.WindingDown);
    }

    function evict() external {
        Layout storage s = VaultStorage.layout();
        if (s.state != VaultState.WindingDown || s.halted()) revert IVault.WrongState();
        if (block.timestamp < s.retiredAt + s.timers.windDown) revert IVault.NotYet();
        _halt(s, "retired");
    }

    function kill(bytes32 reason) external {
        Layout storage s = VaultStorage.layout();
        if (msg.sender != s.admin) revert IVault.NotAllowed();
        VaultState state = s.state;
        // A vault still activating is cancelled, not killed: it never opened.
        if (state != VaultState.Open && state != VaultState.WindingDown && state != VaultState.Frozen) {
            revert IVault.WrongState();
        }
        _halt(s, reason);
        _setState(s, VaultState.Killed);
    }

    /// Once a valuation after the halt shows the stations hold nothing: pay
    /// the fees owed, then stop them. Value is cash from here on.
    function close() external {
        Layout storage s = VaultStorage.layout();
        VaultState state = s.state;
        if (!s.halted() || state == VaultState.Closed || state == VaultState.Cancelled) {
            revert IVault.WrongState();
        }
        if (s.lastPriced == 0) revert IVault.NotYet();
        Valuation storage v = s.latest();
        // Strictly after the halt: nothing can be sent out once it has
        // halted, so a station found empty then stays empty.
        if (v.time <= s.haltedAt || !s.empty(v) || s.moving[s.lastPriced - 1]) {
            revert IVault.NotYet();
        }
        s.passDone();
        // What the last valuation priced is settled at its price; anything
        // asked for after it is settled at the closed vault's (`settleClosed`).
        if (s.valuations[s.valuations.length - 1].status == ValuationStatus.Open || Valuations.owed(s)) {
            revert IVault.Unsettled();
        }
        Valuations.syncHolding(s);
        Valuations.payday(s, s.cash + 1, s.totalSupply + OFFSET, uint64(block.timestamp));
        // The dead shares' part, which no holder can ever redeem, goes to the
        // author who paid for them, into the tank they can take at once.
        uint256 dead = s.balanceOf[VaultStorage.DEAD];
        if (dead != 0) {
            uint256 theirs = s.cash.mulDiv(dead, s.totalSupply);
            s.burn(VaultStorage.DEAD, dead);
            s.cash -= theirs;
            s.tank.balance += theirs;
        }
        _setState(s, VaultState.Closed);
    }

    function redeemClosed(uint256 shares, address receiver) external returns (uint256 assets) {
        Layout storage s = VaultStorage.layout();
        if (s.state != VaultState.Closed) revert IVault.WrongState();
        if (receiver == address(0)) revert IVault.TooSmall();
        assets = s.cash.mulDiv(shares, s.totalSupply);
        s.burn(msg.sender, shares);
        s.cash -= assets;
        s.pay(receiver, assets);
        emit IVault.Withdrawn(0, receiver, shares, assets, 0);
    }

    /// A request nothing priced before the vault closed: a deposit goes back,
    /// a withdrawal is paid as `redeemClosed` pays, a tank fill lapses, and a
    /// claim's request is settled through `claimClosed`.
    function settleClosed(uint256 requestId) external returns (uint256 assets) {
        Layout storage s = VaultStorage.layout();
        if (s.state != VaultState.Closed) revert IVault.WrongState();
        Request storage r = s.requests[requestId - 1];
        RequestKind kind = r.kind;
        if (r.done || kind == RequestKind.Claim) revert IVault.NotAllowed();
        r.done = true;
        if (kind == RequestKind.Deposit) {
            s.escrow -= r.amount;
            address to = s.refundTo(requestId, r.owner);
            s.pay(to, r.amount);
            emit IVault.Refunded(requestId, to, r.amount);
        } else if (kind == RequestKind.Redeem) {
            assets = s.cash.mulDiv(r.amount, s.totalSupply);
            s.pendingShares -= r.amount;
            s.burn(address(this), r.amount);
            s.cash -= assets;
            s.pay(r.owner, assets);
            emit IVault.Withdrawn(requestId, r.owner, r.amount, assets, 0);
        } else if (r.ref == 1) {
            s.tank.fillPending = false;
        }
    }

    function claimClosed(uint256 claimId) external returns (uint256 assets) {
        Layout storage s = VaultStorage.layout();
        if (s.state != VaultState.Closed) revert IVault.WrongState();
        Claim storage claim = s.claims[claimId - 1];
        ClaimState state = claim.state;
        if (state != ClaimState.Pending && state != ClaimState.Waiting && state != ClaimState.Queued) {
            revert IVault.NotAllowed();
        }
        if (state == ClaimState.Queued) {
            s.requests[s.claimRequest[claimId] - 1].done = true;
            s.queuedClaims -= 1;
        }
        assets = s.cash.mulDiv(claim.shares, s.totalSupply);
        s.pendingShares -= claim.shares;
        s.burn(address(this), claim.shares);
        s.cash -= assets;
        claim.state = ClaimState.Paid;
        claim.paid = assets;
        s.pay(claim.owner, assets);
        emit IVault.ClaimPaid(claimId, assets);
    }

    function collectHeld() external returns (uint256 assets) {
        Layout storage s = VaultStorage.layout();
        assets = s.held[msg.sender];
        if (assets == 0) revert IVault.Nothing();
        s.held[msg.sender] = 0;
        s.heldTotal -= assets;
        IERC20(s.asset).safeTransfer(msg.sender, assets);
    }

    // The idle floor --------------------------------------------------------

    /// Raising the floor applies at once; lowering it waits `floorDelay`,
    /// with the old floor in force meanwhile.
    function setIdleFloor(uint16 bps) external {
        Layout storage s = VaultStorage.layout();
        _authorOnly(s);
        if (bps > BPS) revert IVault.NotAllowed();
        uint16 current = uint16(Valuations.floorNow(s));
        s.floorBps = current;
        if (bps >= current) {
            s.floorBps = bps;
            s.pendingFloorBps = 0;
            s.pendingFloorAt = 0;
            emit IVault.IdleFloorSet(bps, uint64(block.timestamp));
        } else {
            s.pendingFloorBps = bps;
            s.pendingFloorAt = uint64(block.timestamp) + s.timers.floorDelay;
            emit IVault.IdleFloorSet(bps, s.pendingFloorAt);
        }
    }

    // Internals -------------------------------------------------------------

    function _authorOnly(Layout storage s) private view {
        if (msg.sender != s.author) revert IVault.NotAllowed();
    }

    /// The agent is told, and keeps its keys for the grace period; after it
    /// anyone may stop the stations and unwind.
    function _halt(Layout storage s, bytes32 reason) private {
        if (s.haltedAt == 0) {
            s.haltedAt = uint64(block.timestamp);
            s.haltReason = reason;
            emit IVault.Halted(reason, s.haltedAt);
        }
    }

    function _setState(Layout storage s, VaultState state) private {
        s.state = state;
        emit IVault.StateChanged(state);
    }
}
