// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationRegistry, ImageKind} from "./interfaces/IAttestationRegistry.sol";
import {ILocalVenue, OrderRequest, Position} from "./interfaces/ILocalVenue.sol";
import {IStrategyRegistry} from "./interfaces/IStrategyRegistry.sol";
import {Claim, ClaimState, Fees, IVault, Tank, VaultState} from "./interfaces/IVault.sol";
import {ERC20} from "openzeppelin/token/ERC20/ERC20.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";
import {ECDSA} from "openzeppelin/utils/cryptography/ECDSA.sol";
import {MessageHashUtils} from "openzeppelin/utils/cryptography/MessageHashUtils.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// Pooled capital behind one strategy version.
///
/// The contract works out what it is worth by reading the chain in the same
/// transaction, so nobody reports a value to it and nobody has to be believed.
/// A depositor can always leave: at once from the idle balance, or as a claim
/// the contract serves itself by closing their fraction of every position
/// inside an exit band. Their shares stay outstanding until it settles, so
/// they bear the cost of that close alongside everyone who stayed.
///
/// Shares follow ERC-4626's accounting -- one asset, pro-rata claims, rounding
/// against whoever is moving -- but `redeem` may book a claim instead of
/// paying, so this is deliberately not a compliant ERC-4626 vault.
contract Vault is IVault, ERC20 {
    using SafeERC20 for IERC20;
    using Math for uint256;

    /// The vault's two accounts, and the only two destinations that exist.
    bytes32 public constant ACCOUNT_IDLE = "idle";
    bytes32 public constant ACCOUNT_VENUE = "venue";
    /// The venue a capital movement or a key approval names.
    bytes32 public constant VENUE_ID = "local";

    /// Shares carry 18 decimals and the asset 6, so a share starts life worth
    /// one dollar. The same offset is the virtual supply that makes the first
    /// deposit safe.
    uint256 private constant SHARE_OFFSET = 1e12;
    uint256 private constant BPS = 10_000;
    uint256 private constant YEAR = 365 days;
    uint256 private constant WAD = 1e18;

    IERC20 public immutable asset;
    ILocalVenue public immutable venue;
    IAttestationRegistry public immutable attestation;
    IStrategyRegistry public immutable strategies;
    address public immutable platform;
    address public immutable admin;

    bytes32 public immutable strategyId;
    uint32 public immutable version;
    bytes32 public immutable sourceHash;
    address public immutable author;
    uint64 public immutable createdAt;
    uint16 public immutable exitBandBps;
    uint32 public immutable freezeAfter;

    VaultState private _state;
    Fees private _fees;
    Tank private _tank;
    uint256 public highWaterMark;
    uint64 public lastCrystallised;
    uint256 public pendingShares;

    Claim[] private _claims;
    address[] private _approvedKeys;
    mapping(address => bool) private _isApprovedKey;

    error WrongState();
    error NotTheAuthor();
    error NotTheAdmin();
    error NotTheBuilder();
    error NotBlessed();
    error NoSuchAccount();
    error NothingToDo();
    error NotYetFrozenable();
    error TooMuch();
    error DayAlreadyCharged();
    error AlreadyPaid();

    struct Init {
        IERC20 asset;
        ILocalVenue venue;
        IAttestationRegistry attestation;
        IStrategyRegistry strategies;
        address platform;
        address admin;
        address author;
        bytes32 strategyId;
        uint32 version;
        bytes32 sourceHash;
        Fees fees;
        uint16 exitBandBps;
        uint32 freezeAfter;
        uint256 dailyUsageCap;
        uint256 tankTarget;
        uint16 autofillBps;
    }

    constructor(Init memory init) ERC20("Shonin vault share", "SHARE") {
        asset = init.asset;
        venue = init.venue;
        attestation = init.attestation;
        strategies = init.strategies;
        platform = init.platform;
        admin = init.admin;
        author = init.author;
        strategyId = init.strategyId;
        version = init.version;
        sourceHash = init.sourceHash;
        createdAt = uint64(block.timestamp);
        _fees = init.fees;
        exitBandBps = init.exitBandBps;
        freezeAfter = init.freezeAfter;
        _tank.dailyCap = init.dailyUsageCap;
        _tank.target = init.tankTarget;
        _tank.autofillBps = init.autofillBps;
        _state = VaultState.Building;
        highWaterMark = 10 ** 6;
        lastCrystallised = uint64(block.timestamp);
    }

    // What it is worth ------------------------------------------------------

    /// The balance on this contract that belongs to depositors. The tank is
    /// the author's, so it never counts.
    function idle() public view returns (uint256) {
        uint256 held = asset.balanceOf(address(this));
        return held > _tank.balance ? held - _tank.balance : 0;
    }

    function totalValue() public view returns (uint256) {
        return idle() + venue.accountValue(address(this));
    }

    function exitValue() public view returns (uint256) {
        return idle() + venue.exitValue(address(this));
    }

    function sharePrice() public view returns (uint256) {
        return _toAssets(WAD, totalValue(), Math.Rounding.Floor);
    }

    function _toShares(
        uint256 assets,
        uint256 total,
        Math.Rounding rounding
    ) private view returns (uint256) {
        return assets.mulDiv(totalSupply() + SHARE_OFFSET, total + 1, rounding);
    }

    function _toAssets(
        uint256 shares,
        uint256 total,
        Math.Rounding rounding
    ) private view returns (uint256) {
        return shares.mulDiv(total + 1, totalSupply() + SHARE_OFFSET, rounding);
    }

    // Depositors ------------------------------------------------------------

    function deposit(uint256 assets, address receiver) external returns (uint256 shares) {
        if (_state != VaultState.Open) revert WrongState();
        if (assets == 0) revert NothingToDo();
        crystallise();

        // The fee never leaves the pool: it lifts the value of every share
        // that stays, so moving in and out around a price change takes
        // nothing from the depositors who didn't.
        uint256 fee = assets.mulDiv(_fees.transactionBps, BPS, Math.Rounding.Ceil);
        // Read the value before the assets arrive, or they would pay for
        // themselves.
        shares = _toShares(assets - fee, totalValue(), Math.Rounding.Floor);

        asset.safeTransferFrom(msg.sender, address(this), assets);
        _mint(receiver, shares);
        emit Deposited(receiver, assets, shares);
    }

    function redeem(uint256 shares, address receiver) external returns (uint256 assets, uint256 claimId) {
        if (_state == VaultState.Building) revert WrongState();
        if (shares == 0) revert NothingToDo();
        crystallise();

        uint256 gross = _toAssets(shares, exitValue(), Math.Rounding.Floor);
        if (gross <= idle()) {
            uint256 fee = gross.mulDiv(_fees.transactionBps, BPS, Math.Rounding.Ceil);
            assets = gross - fee;
            _burn(msg.sender, shares);
            asset.safeTransfer(receiver, assets);
            emit Withdrawn(receiver, shares, assets);
            return (assets, 0);
        }

        // The idle balance cannot cover it, so the shares are marked pending
        // and the contract books a claim. They stay outstanding until it
        // settles: the leaver bears their share of the close, like everyone
        // else in the vault.
        uint256 fraction = shares.mulDiv(WAD, totalSupply(), Math.Rounding.Floor);
        _transfer(msg.sender, address(this), shares);
        pendingShares += shares;
        _claims.push(
            Claim({
                owner: receiver,
                shares: shares,
                fractionClosed: fraction,
                bookedAt: uint64(block.timestamp),
                state: ClaimState.Pending,
                settledFor: 0
            })
        );
        claimId = _claims.length;
        emit ClaimBooked(claimId, receiver, shares, fraction);
    }

    function settle(uint256 claimId) external {
        Claim storage claim = _claims[claimId - 1];
        if (claim.state == ClaimState.Paid) revert AlreadyPaid();
        crystallise();

        uint256 fraction = claim.shares.mulDiv(WAD, totalSupply(), Math.Rounding.Ceil);
        string memory failure = _closeFraction(fraction);
        if (bytes(failure).length != 0) {
            claim.state = ClaimState.Waiting;
            emit ClaimWaiting(claimId, failure);
            return;
        }

        uint256 gross = _toAssets(claim.shares, exitValue(), Math.Rounding.Floor);
        _bringHome(gross);
        if (gross > idle()) {
            claim.state = ClaimState.Waiting;
            emit ClaimWaiting(claimId, "the close did not raise enough");
            return;
        }

        uint256 fee = gross.mulDiv(_fees.transactionBps, BPS, Math.Rounding.Ceil);
        uint256 paid = gross - fee;
        _burn(address(this), claim.shares);
        pendingShares -= claim.shares;
        claim.state = ClaimState.Paid;
        claim.settledFor = paid;
        asset.safeTransfer(claim.owner, paid);
        emit ClaimSettled(claimId, paid);
    }

    function freeze() external {
        uint256 oldest = _oldestOutstandingClaim();
        if (oldest == 0) revert NothingToDo();
        if (block.timestamp < _claims[oldest - 1].bookedAt + freezeAfter) revert NotYetFrozenable();
        if (_state == VaultState.Killed || _state == VaultState.Closed) revert WrongState();

        // No new deposits, and no trading approvals left on the venue, so the
        // only thing anyone can still do here is unwind.
        _state = VaultState.Frozen;
        _revokeEveryKey();
        emit Frozen(oldest);
    }

    // Closing positions -----------------------------------------------------

    /// Close `fraction` of every position with reduce-only orders bounded by
    /// the exit band. Returns why it could not, or an empty string.
    function _closeFraction(uint256 fraction) private returns (string memory) {
        bytes32[] memory held = venue.marketsHeldBy(address(this));
        for (uint256 i = 0; i < held.length; i++) {
            Position memory position = venue.positionOf(address(this), held[i]);
            if (position.size == 0) continue;

            bool isLong = position.size > 0;
            uint256 outstanding = uint256(isLong ? position.size : -position.size);
            uint256 size = outstanding.mulDiv(fraction, WAD, Math.Rounding.Ceil);
            if (size == 0) continue;
            if (size > outstanding) size = outstanding;

            uint256 oracle = venue.marketOf(held[i]).oracle;
            // A long is sold, so its worst acceptable price is below the
            // oracle; a short is bought back above it.
            uint256 limit = isLong
                ? oracle.mulDiv(BPS - exitBandBps, BPS, Math.Rounding.Floor)
                : oracle.mulDiv(BPS + exitBandBps, BPS, Math.Rounding.Ceil);

            OrderRequest[] memory orders = new OrderRequest[](1);
            orders[0] = OrderRequest({
                market: held[i],
                isBuy: !isLong,
                size: size,
                limitPrice: limit,
                reduceOnly: true
            });
            if (!venue.place(address(this), orders)[0].accepted) {
                return "the market is outside the exit band";
            }
        }
        return "";
    }

    /// Bring enough of the venue's cash back to pay `wanted`, if it is there.
    function _bringHome(uint256 wanted) private {
        uint256 here = idle();
        if (here >= wanted) return;
        int256 there = venue.cashOf(address(this));
        if (there <= 0) return;
        uint256 take = wanted - here;
        if (take > uint256(there)) take = uint256(there);
        venue.withdraw(take);
        emit CapitalMoved(ACCOUNT_VENUE, ACCOUNT_IDLE, take);
    }

    function _oldestOutstandingClaim() private view returns (uint256) {
        for (uint256 i = 0; i < _claims.length; i++) {
            if (_claims[i].state == ClaimState.Pending || _claims[i].state == ClaimState.Waiting) {
                return i + 1;
            }
        }
        return 0;
    }

    // Fees ------------------------------------------------------------------

    function crystallise() public {
        if (_state == VaultState.Building || _state == VaultState.Closed) return;
        uint256 elapsed = block.timestamp - lastCrystallised;
        if (elapsed == 0) return;
        lastCrystallised = uint64(block.timestamp);

        uint256 total = totalValue();
        if (total == 0) return;

        uint256 streaming = total.mulDiv(_fees.streamingBps, BPS) * elapsed / YEAR;
        uint256 streamingShares = _mintFee(streaming, total, platform);

        uint256 afterStreaming = total - streaming;
        uint256 valuePerShare = _toAssets(WAD, afterStreaming, Math.Rounding.Floor);
        uint256 authorShares;
        uint256 platformShares;
        if (valuePerShare > highWaterMark) {
            uint256 gain = (valuePerShare - highWaterMark).mulDiv(totalSupply(), WAD, Math.Rounding.Floor);
            uint256 authorFee = gain.mulDiv(_fees.authorBps, BPS, Math.Rounding.Floor);
            uint256 platformFee = gain.mulDiv(_fees.platformBps, BPS, Math.Rounding.Floor);
            authorShares = _mintFee(authorFee, afterStreaming, author);
            platformShares = _mintFee(platformFee, afterStreaming - authorFee, platform);
            _autofill(authorFee);
            highWaterMark = _toAssets(WAD, totalValue(), Math.Rounding.Floor);
        }
        emit Crystallised(streamingShares, authorShares, platformShares, highWaterMark);
    }

    /// Mint the shares that hand `value` out of a pool worth `total` to
    /// `to`. Paying a fee in shares never forces the strategy to sell.
    function _mintFee(uint256 value, uint256 total, address to) private returns (uint256 minted) {
        if (value == 0 || total <= value) return 0;
        minted = value.mulDiv(totalSupply() + SHARE_OFFSET, total - value + 1, Math.Rounding.Floor);
        if (minted == 0) return 0;
        _mint(to, minted);
    }

    /// Move a share of the author's fee into the tank, as far as the idle
    /// balance reaches. It never books a claim: an author's running costs
    /// don't get to jump the queue in front of a depositor.
    function _autofill(uint256 authorFee) private {
        if (_tank.autofillBps == 0) return;
        uint256 want = authorFee.mulDiv(_tank.autofillBps, BPS, Math.Rounding.Floor) + _tank.debt;
        uint256 room = _tank.target > _tank.balance ? _tank.target - _tank.balance : 0;
        if (_tank.debt > room) room = _tank.debt;
        if (want > room) want = room;
        if (want > idle()) want = idle();
        if (want == 0) return;

        uint256 shares = _toShares(want, totalValue(), Math.Rounding.Ceil);
        uint256 held = balanceOf(author);
        if (shares > held) shares = held;
        if (shares == 0) return;
        uint256 moved = _toAssets(shares, totalValue(), Math.Rounding.Floor);
        _burn(author, shares);

        if (_tank.debt >= moved) {
            _tank.debt -= moved;
            asset.safeTransfer(platform, moved);
            emit TankCharged("debt", moved, _tank.debt);
        } else {
            uint256 clearing = _tank.debt;
            _tank.debt = 0;
            if (clearing != 0) asset.safeTransfer(platform, clearing);
            _tank.balance += moved - clearing;
            emit TankFunded(author, moved - clearing);
        }
    }

    // The tank --------------------------------------------------------------

    function fundTank(uint256 assets) external {
        asset.safeTransferFrom(msg.sender, address(this), assets);
        _tank.balance += assets;
        emit TankFunded(msg.sender, assets);
    }

    function submitUsage(uint64 day, uint256 amount, bytes calldata signature) external {
        if (day <= _tank.lastChargedDay) revert DayAlreadyCharged();
        bytes32 digest = MessageHashUtils.toEthSignedMessageHash(usageDigest(day, amount));
        (address signer, ECDSA.RecoverError err,) = ECDSA.tryRecover(digest, signature);
        if (err != ECDSA.RecoverError.NoError) revert NotBlessed();
        if (!attestation.isBlessedFor(signer, address(this), bundleHash())) revert NotBlessed();

        // The statement says what a day cost; the contract believes it only up
        // to the allowances the author declared at the published prices.
        uint256 charge = amount > _tank.dailyCap ? _tank.dailyCap : amount;
        _tank.lastChargedDay = day;
        _tank.lastCharge = charge;

        uint256 paid = charge > _tank.balance ? _tank.balance : charge;
        _tank.balance -= paid;
        _tank.debt += charge - paid;
        if (paid != 0) asset.safeTransfer(platform, paid);
        emit TankCharged("usage", charge, _tank.debt);
    }

    function setAutofill(uint16 bps) external {
        if (msg.sender != author) revert NotTheAuthor();
        if (bps > BPS) revert TooMuch();
        _tank.autofillBps = bps;
    }

    function usageDigest(uint64 day, uint256 amount) public view returns (bytes32) {
        return keccak256(abi.encode(block.chainid, address(this), "shonin-usage", day, amount));
    }

    // The strategy's own key ------------------------------------------------

    function moveCapital(bytes32 from, bytes32 to, uint256 amount) external {
        if (!attestation.isBlessedFor(msg.sender, address(this), bundleHash())) revert NotBlessed();
        if (_state != VaultState.Open) revert WrongState();

        if (from == ACCOUNT_IDLE && to == ACCOUNT_VENUE) {
            if (amount > idle()) revert TooMuch();
            asset.forceApprove(address(venue), amount);
            venue.deposit(amount);
        } else if (from == ACCOUNT_VENUE && to == ACCOUNT_IDLE) {
            venue.withdraw(amount);
        } else {
            revert NoSuchAccount();
        }
        emit CapitalMoved(from, to, amount);
    }

    // Lifecycle -------------------------------------------------------------

    function open() external {
        if (_state != VaultState.Building) revert WrongState();
        bytes32 bundle = bundleHash();
        if (bundle == bytes32(0)) revert NothingToDo();
        _state = VaultState.Open;
        lastCrystallised = uint64(block.timestamp);
        emit Opened(bundle);
    }

    function failBuild(bytes32 reasonHash) external {
        if (_state != VaultState.Building) revert WrongState();
        if (!attestation.isBlessedAs(msg.sender, ImageKind.Builder)) {
            revert NotTheBuilder();
        }
        uint256 refund = _tank.balance;
        _tank.balance = 0;
        _state = VaultState.Closed;
        if (refund != 0) asset.safeTransfer(author, refund);
        emit BuildFailed(reasonHash);
        emit Closed();
    }

    function kill() external {
        if (msg.sender != admin) revert NotTheAdmin();
        if (_state != VaultState.Open && _state != VaultState.Frozen) revert WrongState();
        crystallise();

        // A kill moves money in one direction only: out of positions and back
        // to the people who own it.
        _closeFraction(WAD);
        int256 there = venue.cashOf(address(this));
        if (there > 0) {
            venue.withdraw(uint256(there));
            emit CapitalMoved(ACCOUNT_VENUE, ACCOUNT_IDLE, uint256(there));
        }
        _revokeEveryKey();
        _state = VaultState.Killed;
        emit Killed(msg.sender);
    }

    // Venue approvals -------------------------------------------------------

    function approveKey(bytes32 venueId, address key) external {
        if (venueId != VENUE_ID) revert NoSuchAccount();
        if (_state != VaultState.Open) revert WrongState();
        if (!attestation.isBlessedFor(key, address(this), bundleHash())) revert NotBlessed();
        uint64 until = attestation.keyInfo(key).blessedUntil;
        venue.approveAgent(key, until);
        if (!_isApprovedKey[key]) {
            _isApprovedKey[key] = true;
            _approvedKeys.push(key);
        }
        emit KeyApproved(venueId, key, until);
    }

    function revokeKey(bytes32 venueId, address key) external {
        if (venueId != VENUE_ID) revert NoSuchAccount();
        bool stillBlessed = attestation.isBlessedFor(key, address(this), bundleHash());
        bool unwindingOnly = _state == VaultState.Frozen || _state == VaultState.Killed;
        // Anyone may revoke, and the contract obeys only what the registry
        // says: a key whose blessing still stands cannot be cut off by a
        // passer-by, and one whose blessing has lapsed cannot be kept.
        if (stillBlessed && !unwindingOnly) revert NotBlessed();
        venue.revokeAgent(key);
        _isApprovedKey[key] = false;
        emit KeyRevoked(venueId, key);
    }

    function _revokeEveryKey() private {
        for (uint256 i = 0; i < _approvedKeys.length; i++) {
            address key = _approvedKeys[i];
            if (!_isApprovedKey[key]) continue;
            _isApprovedKey[key] = false;
            venue.revokeAgent(key);
            emit KeyRevoked(VENUE_ID, key);
        }
    }

    // What the page shows ---------------------------------------------------

    function bundleHash() public view returns (bytes32) {
        return strategies.bundleHashOf(strategyId, version);
    }

    function state() external view returns (VaultState) {
        return _state;
    }

    function fees() external view returns (Fees memory) {
        return _fees;
    }

    function tank() external view returns (Tank memory) {
        return _tank;
    }

    function claimCount() external view returns (uint256) {
        return _claims.length;
    }

    function claimAt(uint256 claimId) external view returns (Claim memory) {
        return _claims[claimId - 1];
    }

    function approvedKeys() external view returns (address[] memory) {
        return _approvedKeys;
    }
}
