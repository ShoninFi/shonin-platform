// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Allowances} from "./interfaces/IPrices.sol";
import {
    Claim,
    Fees,
    IVault,
    Request,
    StationPost,
    Tank,
    Timers,
    Valuation,
    VaultInit,
    VaultState
} from "./interfaces/IVault.sol";
import {Claims} from "./vault/Claims.sol";
import {Posts} from "./vault/Posts.sol";
import {Upkeep} from "./vault/Upkeep.sol";
import {Valuations} from "./vault/Valuations.sol";
import {Layout, VaultStorage} from "./vault/VaultStorage.sol";

import {ReentrancyGuardTransient} from "openzeppelin/utils/ReentrancyGuardTransient.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// Pooled capital behind one strategy.
///
/// Every vault is a clone of this one, created by the factory when an author
/// activates a strategy. It holds shares, cash, the tank, requests and claims,
/// and never trades: its home station does. What it is worth comes from the
/// last valuation, never from its own token balance, so money sent to it by
/// anyone else is ignored. The work is split into libraries over one storage
/// layout so each part fits the size limit; this contract is the door.
contract Vault is IVault, ReentrancyGuardTransient {
    using VaultStorage for Layout;
    using Math for uint256;

    /// The implementation itself is never a vault.
    constructor() {
        VaultStorage.layout().factory = address(1);
    }

    function initialize(VaultInit calldata init, uint256 tankFunding, uint256 deadAssets) external {
        Upkeep.initialize(init, tankFunding, deadAssets);
    }

    // Depositors ------------------------------------------------------------

    function requestDeposit(
        uint256 assets,
        address receiver,
        bytes calldata statement
    ) external nonReentrant returns (uint256) {
        if (statement.length != 0) Upkeep.recordStatement(receiver, statement);
        return Valuations.requestDeposit(assets, receiver);
    }

    function requestRedeem(uint256 shares, address receiver) external nonReentrant returns (uint256) {
        return Valuations.requestRedeem(shares, receiver);
    }

    function recordValuation() external nonReentrant {
        Valuations.recordValuation();
    }

    function postStation(
        uint64 time,
        uint256 stationChain,
        StationPost calldata post,
        bytes calldata signature
    ) external nonReentrant {
        Posts.postStation(time, stationChain, post, signature);
    }

    function closeValuation(uint64 time) external nonReentrant {
        Posts.closeValuation(time);
    }

    function postDigest(
        uint64 time,
        uint256 stationChain,
        StationPost calldata post
    ) external view returns (bytes32) {
        return Posts.postDigest(time, stationChain, post);
    }

    function settle(uint256 max) external nonReentrant returns (uint256) {
        return Valuations.settle(max);
    }

    function settleClaim(uint256 claimId) external nonReentrant {
        Claims.settleClaim(claimId);
    }

    function freeze(uint256 claimId) external nonReentrant {
        Upkeep.freeze(claimId);
    }

    function takeProRata(uint256 requestId) external nonReentrant {
        Valuations.takeProRata(requestId);
    }

    function claimInstead(uint256 requestId) external nonReentrant returns (uint256) {
        return Claims.claimInstead(requestId);
    }

    function settleClosed(uint256 requestId) external nonReentrant returns (uint256) {
        return Upkeep.settleClosed(requestId);
    }

    function takeProRataClaim(uint256 claimId) external nonReentrant {
        Claims.takeProRataClaim(claimId);
    }

    function reportExitCost(
        uint256 claimId,
        uint256 cost,
        address payTo,
        bytes calldata signature
    ) external nonReentrant {
        Posts.reportExitCost(claimId, cost, payTo, signature);
    }

    function exitCostDigest(uint256 claimId, uint256 cost, address payTo) external view returns (bytes32) {
        return Posts.exitCostDigest(claimId, cost, payTo);
    }

    function exitCostOf(uint256 claimId) external view returns (uint256 cost, address payTo) {
        Layout storage s = VaultStorage.layout();
        return (s.exitCost[claimId], s.exitCostTo[claimId]);
    }

    function maxExitCost() external view returns (uint256) {
        return VaultStorage.layout().maxExitCost;
    }

    function collectUnits() external nonReentrant returns (uint256) {
        return Valuations.collectUnits();
    }

    function serveProRata(address holder) external nonReentrant {
        Valuations.serveProRata(holder);
    }

    function redeemClosed(uint256 shares, address receiver) external nonReentrant returns (uint256) {
        return Upkeep.redeemClosed(shares, receiver);
    }

    function claimClosed(uint256 claimId) external nonReentrant returns (uint256) {
        return Upkeep.claimClosed(claimId);
    }

    function collectHeld() external nonReentrant returns (uint256) {
        return Upkeep.collectHeld();
    }

    // The home station ------------------------------------------------------

    /// Not guarded: the station sends money home through this while the vault
    /// is asking it to unwind.
    function toStation(uint256 amount) external {
        Valuations.toStation(amount);
    }

    function fromStation(uint256 amount) external {
        Valuations.fromStation(amount);
    }

    function haltedAt() external view returns (uint64) {
        return VaultStorage.layout().haltedAt;
    }

    function haltGrace() external view returns (uint64) {
        return VaultStorage.layout().timers.haltGrace;
    }

    // The tank --------------------------------------------------------------

    function fundTank(uint256 assets) external nonReentrant {
        Upkeep.fundTank(assets);
    }

    function keep(uint256 total, uint64 asOf, bytes calldata signature) external nonReentrant {
        Upkeep.keep(total, asOf, signature);
    }

    function withdrawTank(
        uint256 assets,
        uint256 total,
        uint64 asOf,
        bytes calldata signature
    ) external nonReentrant {
        Upkeep.withdrawTank(assets, total, asOf, signature);
    }

    function requestTankWithdrawal() external nonReentrant {
        Upkeep.requestTankWithdrawal();
    }

    function withdrawTankUnsigned(uint256 assets) external nonReentrant {
        Upkeep.withdrawTankUnsigned(assets);
    }

    function topUpFromShares(uint256 shares) external nonReentrant {
        Upkeep.topUpFromShares(shares);
    }

    function setTankKeep(uint256 assets) external nonReentrant {
        Upkeep.setTankKeep(assets);
    }

    function setAllowances(Allowances calldata allowances_) external nonReentrant {
        Upkeep.setAllowances(allowances_);
    }

    function usageDigest(uint256 total, uint64 asOf) external view returns (bytes32) {
        return Upkeep.usageDigest(total, asOf);
    }

    // Who may deposit -------------------------------------------------------

    function recordStatement(address depositor, bytes calldata signature) external nonReentrant {
        Upkeep.recordStatement(depositor, signature);
    }

    function refreshRestricted() external nonReentrant {
        Upkeep.refreshRestricted();
    }

    function statementDigest(address depositor) external view returns (bytes32) {
        return Upkeep.statementDigest(depositor);
    }

    function restricted() external view returns (bytes32[] memory) {
        return VaultStorage.layout().restricted;
    }

    function restrictedText() external view returns (string memory) {
        return Upkeep.restrictedText();
    }

    function statementOf(address depositor) external view returns (bytes32) {
        return VaultStorage.layout().statementOf[depositor];
    }

    // The author and the admin ----------------------------------------------

    function setIdleFloor(uint16 bps) external nonReentrant {
        Upkeep.setIdleFloor(bps);
    }

    function retire() external nonReentrant {
        Upkeep.retire();
    }

    function kill() external nonReentrant {
        Upkeep.kill("killed");
    }

    /// The root key is lost: stop, so positions are unwound and everyone is
    /// paid while the machines that hold the keys still run.
    function windDownSealed() external nonReentrant {
        Upkeep.kill("sealed");
    }

    // Lifecycle, open to anyone ---------------------------------------------

    function confirmStations() external nonReentrant {
        Upkeep.confirmStations();
    }

    function cancel() external nonReentrant {
        Upkeep.cancel();
    }

    function evict() external nonReentrant {
        Upkeep.evict();
    }

    function close() external nonReentrant {
        Upkeep.close();
    }

    // What it is worth, at the last valuation -------------------------------

    function sharePrice() external view returns (uint256) {
        Layout storage s = VaultStorage.layout();
        if (s.lastPriced == 0) return 1e6;
        Valuation storage v = s.latest();
        return (v.gross - v.fee).mulDiv(1e18, v.supply);
    }

    function totalValue() external view returns (uint256) {
        Layout storage s = VaultStorage.layout();
        return s.lastPriced == 0 ? s.cash : s.latest().gross - 1;
    }

    function exitValue() external view returns (uint256) {
        Layout storage s = VaultStorage.layout();
        return s.lastPriced == 0 ? s.cash : s.latest().grossExit - 1;
    }

    // Shares, which never move between holders -------------------------------

    function name() external pure returns (string memory) {
        return "Shonin vault share";
    }

    function symbol() external pure returns (string memory) {
        return "SHARE";
    }

    function decimals() external pure returns (uint8) {
        return 18;
    }

    function totalSupply() external view returns (uint256) {
        return VaultStorage.layout().totalSupply;
    }

    function balanceOf(address owner) external view returns (uint256) {
        return VaultStorage.layout().balanceOf[owner];
    }

    function transfer(address, uint256) external pure returns (bool) {
        revert NotTransferable();
    }

    function transferFrom(address, address, uint256) external pure returns (bool) {
        revert NotTransferable();
    }

    function approve(address, uint256) external pure returns (bool) {
        revert NotTransferable();
    }

    function allowance(address, address) external pure returns (uint256) {
        return 0;
    }

    // What the page shows ---------------------------------------------------

    function state() external view returns (VaultState) {
        return VaultStorage.layout().state;
    }

    function fees() external view returns (Fees memory) {
        return VaultStorage.layout().fees;
    }

    function timers() external view returns (Timers memory) {
        return VaultStorage.layout().timers;
    }

    function tank() external view returns (Tank memory) {
        return VaultStorage.layout().tank;
    }

    function allowances() external view returns (Allowances memory) {
        return VaultStorage.layout().allowances;
    }

    function cash() external view returns (uint256) {
        return VaultStorage.layout().cash;
    }

    function escrow() external view returns (uint256) {
        return VaultStorage.layout().escrow;
    }

    function mark() external view returns (uint256) {
        return VaultStorage.layout().mark;
    }

    function lastPayment() external view returns (uint64) {
        return VaultStorage.layout().lastPayment;
    }

    function idleFloor() external view returns (uint16, uint16, uint64) {
        Layout storage s = VaultStorage.layout();
        return (s.floorBps, s.pendingFloorBps, s.pendingFloorAt);
    }

    function requestCount() external view returns (uint256) {
        return VaultStorage.layout().requests.length;
    }

    function requestAt(uint256 requestId) external view returns (Request memory) {
        return VaultStorage.layout().requests[requestId - 1];
    }

    function valuationCount() external view returns (uint256) {
        return VaultStorage.layout().valuations.length;
    }

    function valuationAt(uint256 index) external view returns (Valuation memory) {
        return VaultStorage.layout().valuations[index];
    }

    function claimCount() external view returns (uint256) {
        return VaultStorage.layout().claims.length;
    }

    function claimAt(uint256 claimId) external view returns (Claim memory) {
        return VaultStorage.layout().claims[claimId - 1];
    }

    function units() external view returns (uint256, uint256) {
        Layout storage s = VaultStorage.layout();
        return (s.liveUnits(), s.unitReserve);
    }

    function unitsOf(address holder) external view returns (uint256 held, uint256 claimable) {
        Layout storage s = VaultStorage.layout();
        uint256 raw = s.unitsOf[holder];
        held = raw.mulDiv(s.scale(), VaultStorage.WAD);
        claimable = s.unitCredit[holder] + raw.mulDiv(s.perUnit, VaultStorage.UNIT_SCALE) - s.unitDebt[holder];
    }

    function heldFor(address owner) external view returns (uint256) {
        return VaultStorage.layout().held[owner];
    }

    function homeStation() external view returns (address) {
        return VaultStorage.layout().homeStation;
    }

    function strategyId() external view returns (uint256) {
        return VaultStorage.layout().strategyId;
    }

    function bundleHash() external view returns (bytes32) {
        return VaultStorage.layout().bundleHash;
    }

    function author() external view returns (address) {
        return VaultStorage.layout().author;
    }

    function exitBandBps() external view returns (uint16) {
        return VaultStorage.layout().exitBandBps;
    }

    function createdAt() external view returns (uint64) {
        return VaultStorage.layout().createdAt;
    }
}
