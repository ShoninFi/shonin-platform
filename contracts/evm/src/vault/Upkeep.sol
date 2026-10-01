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

    /// Read the same-chain drivers' lists again. The list only ever grows,
    /// and a depositor signs again for the longer one.
    function refreshRestricted() external {
        Layout storage s = VaultStorage.layout();
        IAttestationRegistry registry = IAttestationRegistry(s.attestation);
        bool added;
        for (uint256 i = 0; i < s.drivers.length; i++) {
            address implementation = registry.unwindDriver(s.drivers[i]);
            if (implementation == address(0)) continue;
            bytes32[] memory codes = IDriver(implementation).restricted();
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
                && state != VaultState.Closed && state != VaultState.Cancelled
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
    /// without one.
    function requestTankWithdrawal() external {
        Layout storage s = VaultStorage.layout();
        _authorOnly(s);
        s.tank.escapeRequestedAt = uint64(block.timestamp);
    }

    function withdrawTankUnsigned(uint256 assets) external {
        Layout storage s = VaultStorage.layout();
        _authorOnly(s);
        if (s.state != VaultState.Closed && s.state != VaultState.Cancelled) {
            uint64 asked = s.tank.escapeRequestedAt;
            if (asked == 0 || block.timestamp < asked + s.timers.tankEscapeDelay) revert IVault.NotYet();
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
        if (state == VaultState.Activating || state == VaultState.Closed || state == VaultState.Cancelled) {
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

    /// Close the leaver's fraction of every position inside the band and
    /// bring it home, then price the claim at the next valuation. Anyone may
    /// try; trying again while it is queued is refused, so nobody can make
    /// the vault close more than the leaver's share.
    function settleClaim(uint256 claimId) external {
        Layout storage s = VaultStorage.layout();
        Claim storage claim = s.claims[claimId - 1];
        if (claim.state != ClaimState.Pending && claim.state != ClaimState.Waiting) {
            revert IVault.NotAllowed();
        }
        if (s.state == VaultState.Closed || s.state == VaultState.Cancelled) revert IVault.WrongState();
        // The leaver's part of what the shares own of the stations.
        uint256 fraction = claim.shares.mulDiv(WAD - s.liveUnits(), s.totalSupply, Math.Rounding.Ceil);
        if (fraction > WAD) fraction = WAD;
        if (fraction == 0) fraction = 1;
        uint256 home;
        bool blocked;
        try IStation(s.homeStation).unwind(fraction) returns (uint256 sent, bool outside) {
            (home, blocked) = (sent, outside);
        } catch (bytes memory reason) {
            // Proceeds a venue keeps as margin leave the close undone, so a
            // claim that can't come home sells nothing however often it is
            // tried.
            if (bytes4(reason) != IStation.ShortSweep.selector) {
                assembly ("memory-safe") {
                    revert(add(reason, 32), mload(reason))
                }
            }
        }
        if (blocked) {
            // A market outside the band is a real try: the clock restarts.
            claim.state = ClaimState.Waiting;
            claim.lastAttemptAt = uint64(block.timestamp);
        } else if (home == 0) {
            // A close that brought nothing home served nobody, so it doesn't
            // count as a try, and the freeze clock keeps running.
            claim.state = ClaimState.Waiting;
            blocked = true;
        } else {
            // What the claim is expected to be paid is set aside, so nothing
            // priced ahead of it can take it. The freeze clock is left alone:
            // only a claim that is paid has been served.
            uint256 expected = s.lastPriced == 0 ? home : _expectedPayout(s, claim.shares);
            uint256 reserve = expected < s.free() ? expected : s.free();
            s.claimReserved[claimId] = reserve;
            s.claimHome[claimId] = home;
            s.reservedCash += reserve;
            claim.state = ClaimState.Queued;
            s.queuedClaims += 1;
            s.claimRequest[claimId] = s.queue(claim.owner, RequestKind.Claim, claim.shares, claimId);
        }
        emit IVault.ClaimTried(claimId, blocked);
    }

    function _expectedPayout(Layout storage s, uint256 shares) private view returns (uint256 payout) {
        (,, payout) = Valuations.exitOf(s, s.latest(), shares);
    }

    /// A claim nobody has tried to serve for `freezeAfter` freezes the vault.
    function freeze(uint256 claimId) external {
        Layout storage s = VaultStorage.layout();
        Claim storage claim = s.claims[claimId - 1];
        ClaimState state = claim.state;
        if (state != ClaimState.Pending && state != ClaimState.Waiting && state != ClaimState.Queued) {
            revert IVault.NotAllowed();
        }
        uint64 since = claim.lastAttemptAt > claim.bookedAt ? claim.lastAttemptAt : claim.bookedAt;
        if (block.timestamp < since + s.timers.freezeAfter) revert IVault.NotYet();
        if (!s.live()) revert IVault.WrongState();
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
        if (
            state != VaultState.Activating && state != VaultState.Open && state != VaultState.WindingDown
                && state != VaultState.Frozen
        ) revert IVault.WrongState();
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
        if (v.time <= s.haltedAt || v.gross != v.cash + 1) revert IVault.NotYet();
        if (s.valuations[s.valuations.length - 1].status == ValuationStatus.Open) revert IVault.Unsettled();
        for (uint256 i = s.requestCursor; i < s.requests.length; i++) {
            if (!s.requests[i].done) revert IVault.Unsettled();
        }
        Valuations.syncHolding(s);
        Valuations.payday(s, s.cash + 1, s.totalSupply + OFFSET, uint64(block.timestamp));
        _setState(s, VaultState.Closed);
    }

    function redeemClosed(uint256 shares, address receiver) external returns (uint256 assets) {
        Layout storage s = VaultStorage.layout();
        if (s.state != VaultState.Closed) revert IVault.WrongState();
        assets = s.cash.mulDiv(shares, s.totalSupply);
        s.burn(msg.sender, shares);
        s.cash -= assets;
        s.pay(receiver, assets);
        emit IVault.Withdrawn(0, receiver, shares, assets, 0);
    }

    function claimClosed(uint256 claimId) external returns (uint256 assets) {
        Layout storage s = VaultStorage.layout();
        if (s.state != VaultState.Closed) revert IVault.WrongState();
        Claim storage claim = s.claims[claimId - 1];
        ClaimState state = claim.state;
        if (state != ClaimState.Pending && state != ClaimState.Waiting && state != ClaimState.Queued) {
            revert IVault.NotAllowed();
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
