// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Allowances} from "./IPrices.sol";
import {IStationVault} from "./IStation.sol";

/// Where a vault is in its life.
enum VaultState {
    None,
    /// Created, waiting for its stations on other chains to exist.
    Activating,
    /// Taking deposits, trading.
    Open,
    /// Retired by its author: no deposits; the strategy runs until evicted.
    WindingDown,
    /// A claim went untried too long: no deposits, claims still served.
    Frozen,
    /// Stopped by the admin key.
    Killed,
    /// Nothing at the stations: value is cash, and fees have stopped.
    Closed,
    /// Never opened; its tank went back to its author.
    Cancelled
}

/// What a request asks for.
enum RequestKind {
    /// Assets in escrow, for shares at a later valuation.
    Deposit,
    /// Shares held by the vault, for assets at a later valuation.
    Redeem,
    /// A claim's second attempt, after its fraction was closed.
    Claim,
    /// The author's shares, redeemed into the tank.
    TankFill
}

/// One request, settled at the first priced valuation at or after `earliest`.
struct Request {
    address owner;
    RequestKind kind;
    /// The first valuation time it can settle at.
    uint64 earliest;
    /// Settled, or taken through the pro-rata fallback.
    bool done;
    /// Assets for a deposit; shares for a redemption; for a tank fill, shares
    /// when `ref` is zero and dollars when it is one.
    uint256 amount;
    /// A claim's id, or a tank fill's unit.
    uint256 ref;
}

enum ValuationStatus {
    None,
    /// Recorded, waiting for posts from stations on other chains.
    Open,
    Priced,
    /// A station never posted: its requests roll on to the next valuation.
    Void
}

/// One valuation's snapshot: what every request it settles is priced at.
struct Valuation {
    uint64 time;
    ValuationStatus status;
    /// Cash and every station at mark, counted money in transit, plus one.
    uint256 gross;
    /// The same at exit-side prices, never above `gross`.
    uint256 grossExit;
    /// Every share, plus the virtual offset.
    uint256 supply;
    /// The profit fee owed on `gross`, and on `grossExit`.
    uint256 fee;
    uint256 feeExit;
    /// Cash at the snapshot, for the idle floor.
    uint256 cash;
    /// Stations on other chains that have not posted yet.
    uint32 postsMissing;
}

enum ClaimState {
    None,
    /// Booked: the leaver's shares are pending and outstanding.
    Pending,
    /// The last attempt found a market outside the exit band.
    Waiting,
    /// Its fraction was closed; it is priced at the next valuation.
    Queued,
    Paid,
    /// Taken through the pro-rata fallback.
    Fallback
}

/// Money a station sent over a bridge that has not landed yet.
struct Transit {
    uint256 amount;
    uint64 sentAt;
}

/// What a station on another chain reports for one valuation, signed by a
/// key blessed for the vault.
struct StationPost {
    /// What the station holds, at mark and at exit-side prices, computed on
    /// its own chain.
    uint256 mark;
    uint256 exit;
    /// The station chain's first block at or after the valuation's time.
    uint64 blockNumber;
    /// Sends from this station that have not landed.
    Transit[] transit;
    /// The home station's sends to this chain that have not landed, by their
    /// place in its list, in increasing order. Bridges may land out of
    /// order, so each one still on its way is named.
    uint64[] inFlightFromHome;
}

/// A withdrawal the cash could not pay.
struct Claim {
    address owner;
    uint256 shares;
    uint64 bookedAt;
    uint64 lastAttemptAt;
    ClaimState state;
    /// What it paid.
    uint256 paid;
}

/// The schedule a vault keeps for life.
struct Fees {
    /// A year, on value, to the platform.
    uint16 holdingBps;
    /// Of new profit, to the author.
    uint16 authorBps;
    /// Of the same profit, to the platform.
    uint16 platformBps;
    /// On every deposit and withdrawal, kept in the pool.
    uint16 transactionBps;
}

/// Every timer a vault runs on. Deployment configuration, never constants:
/// the local chains run an hour of chain time per block.
struct Timers {
    uint64 slotInterval;
    uint64 slotLead;
    uint64 slotWindow;
    uint64 postDeadline;
    uint64 transitTimeout;
    uint64 fallbackAfter;
    uint64 usageFreshness;
    uint64 tankEscapeDelay;
    uint64 floorDelay;
    uint64 haltGrace;
    uint64 freezeAfter;
    uint64 windDown;
    uint64 activationWindow;
}

/// The author's dollars for running costs. Never counted in value.
struct Tank {
    uint256 balance;
    /// "Keep my tank at": what auto-fill tops it back up to. Zero is off.
    uint256 keepAt;
    /// The running usage total collected so far, and when it was signed.
    uint256 usageSettled;
    uint64 usageAsOf;
    /// Where the next collection's cap is measured from.
    uint64 lastCollection;
    /// When the author asked to withdraw without a signed total.
    uint64 escapeRequestedAt;
    /// Cap earned under earlier allowances since the last collection.
    uint256 capCarried;
    /// A tank fill is waiting in the queue.
    bool fillPending;
}

/// What the factory creates a vault with.
struct VaultInit {
    address asset;
    address attestation;
    address prices;
    address homeStation;
    address platform;
    address admin;
    address author;
    uint256 strategyId;
    bytes32 bundleHash;
    Fees fees;
    uint16 exitBandBps;
    uint16 idleFloorBps;
    uint256 minimumDeposit;
    Timers timers;
    Allowances allowances;
    uint256 tankKeep;
    /// Who can't deposit, sorted, without repeats.
    bytes32[] restricted;
    /// The chains the strategy trades on besides the vault's own.
    uint256[] otherChains;
    /// The most an unwinder may report as one claim's exit cost.
    uint256 maxExitCost;
}

/// Pooled capital behind one strategy.
///
/// It never trades: its home station does. Deposits and withdrawals are
/// requests priced at a later valuation, so nobody who moves can pick the
/// price they move at, and everything one valuation settles is priced at its
/// snapshot. Fees are measured at every valuation and paid on the first one
/// on or after Monday. The tank is the author's, and only ever pays the
/// running costs the author's own allowances cap.
///
/// Shares are not transferable.
interface IVault is IStationVault {
    /// Shares moving, as an ERC-20 reports it, so wallets and indexers see
    /// mints, burns and fee shares.
    event Transfer(address indexed from, address indexed to, uint256 value);
    event Requested(
        uint256 indexed requestId, address indexed owner, RequestKind kind, uint64 earliest, uint256 amount
    );
    event Deposited(uint256 indexed requestId, address indexed owner, uint256 assets, uint256 shares);
    event Refunded(uint256 indexed requestId, address indexed owner, uint256 assets);
    event Withdrawn(
        uint256 indexed requestId, address indexed owner, uint256 shares, uint256 assets, uint256 feeShares
    );
    event Valued(
        uint256 indexed index, uint64 time, uint256 gross, uint256 grossExit, uint256 supply, uint256 fee
    );
    event ValuationOpened(uint256 indexed index, uint64 time, uint32 postsMissing);
    event ValuationVoided(uint256 indexed index, uint64 time);
    event StationPosted(
        uint256 indexed index, uint256 indexed stationChain, address signer, uint256 mark, uint256 exit
    );
    event ExitCostReported(uint256 indexed claimId, uint256 cost, address payTo);
    event HoldingSynced(uint256 shares);
    event FeesPaid(uint256 authorShares, uint256 platformShares, uint256 holdingShares, uint256 mark);
    event MarkRaised(uint256 mark);
    event ClaimBooked(uint256 indexed claimId, address indexed owner, uint256 shares);
    event ClaimTried(uint256 indexed claimId, bool blocked);
    event ClaimPaid(uint256 indexed claimId, uint256 assets);
    event ProRataTaken(address indexed owner, uint256 shares, uint256 assets, uint256 units);
    event UnitsPaid(address indexed owner, uint256 assets);
    event TankFunded(address indexed from, uint256 assets);
    event TankCharged(uint256 billed, uint256 charged, uint256 total);
    event TankWithdrawn(uint256 assets);
    event TankFilled(uint256 shares, uint256 assets);
    event TankKeepSet(uint256 assets);
    event AllowancesSet(Allowances allowances);
    event IdleFloorSet(uint16 bps, uint64 effectiveAt);
    event StateChanged(VaultState state);
    event Halted(bytes32 reason, uint64 haltedAt);
    event StatementRecorded(address indexed depositor, bytes32 restrictedHash);
    event RestrictedAdded(bytes32 code);
    event ToStation(uint256 assets);
    event FromStation(uint256 assets, uint256 toUnits);
    event Held(address indexed to, uint256 assets);

    error WrongState();
    error NotAllowed();
    error TooSmall();
    error NoStatement();
    error BadSignature();
    error NotBlessed();
    error OutsideWindow();
    error Unsettled();
    error AlreadyValued();
    error NotYet();
    error Nothing();
    error BelowFloor();
    error Stale();
    error NotTransferable();

    function initialize(VaultInit calldata init, uint256 tankFunding, uint256 deadAssets) external;

    // Depositors
    function requestDeposit(
        uint256 assets,
        address receiver,
        bytes calldata statement
    ) external returns (uint256 requestId);
    function requestRedeem(uint256 shares, address receiver) external returns (uint256 requestId);
    function recordValuation() external;
    function postStation(
        uint64 time,
        uint256 stationChain,
        StationPost calldata post,
        bytes calldata signature
    ) external;
    function closeValuation(uint64 time) external;
    function postDigest(
        uint64 time,
        uint256 stationChain,
        StationPost calldata post
    ) external view returns (bytes32);
    function settle(uint256 max) external returns (uint256 settled);
    function settleClaim(uint256 claimId) external;
    function freeze(uint256 claimId) external;
    function takeProRata(uint256 requestId) external;
    function takeProRataClaim(uint256 claimId) external;
    /// An unwinder's cost of bringing a claim's fraction home from another
    /// chain: taken from the leaver's payout and paid to `payTo`.
    function reportExitCost(
        uint256 claimId,
        uint256 cost,
        address payTo,
        bytes calldata signature
    ) external;
    function exitCostDigest(uint256 claimId, uint256 cost, address payTo) external view returns (bytes32);
    function exitCostOf(uint256 claimId) external view returns (uint256 cost, address payTo);
    function maxExitCost() external view returns (uint256);
    function collectUnits() external returns (uint256);
    function serveProRata(address holder) external;
    function redeemClosed(uint256 shares, address receiver) external returns (uint256);
    function claimClosed(uint256 claimId) external returns (uint256);
    function collectHeld() external returns (uint256);

    // The tank
    function fundTank(uint256 assets) external;
    function keep(uint256 total, uint64 asOf, bytes calldata signature) external;
    function withdrawTank(uint256 assets, uint256 total, uint64 asOf, bytes calldata signature) external;
    function requestTankWithdrawal() external;
    function withdrawTankUnsigned(uint256 assets) external;
    function topUpFromShares(uint256 shares) external;
    function setTankKeep(uint256 assets) external;
    function setAllowances(Allowances calldata allowances) external;
    function usageDigest(uint256 total, uint64 asOf) external view returns (bytes32);

    // Who may deposit
    function recordStatement(address depositor, bytes calldata signature) external;
    function refreshRestricted() external;
    function statementDigest(address depositor) external view returns (bytes32);
    function restricted() external view returns (bytes32[] memory);
    function restrictedText() external view returns (string memory);
    function statementOf(address depositor) external view returns (bytes32);

    // The author and the admin
    function setIdleFloor(uint16 bps) external;
    function retire() external;
    function kill() external;
    function windDownSealed() external;

    // Lifecycle, open to anyone
    function confirmStations() external;
    function cancel() external;
    function evict() external;
    function close() external;

    // What it is worth
    function sharePrice() external view returns (uint256);
    function totalValue() external view returns (uint256);
    function exitValue() external view returns (uint256);

    // Shares
    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function decimals() external view returns (uint8);
    function totalSupply() external view returns (uint256);
    function balanceOf(address owner) external view returns (uint256);

    // What the page shows
    function state() external view returns (VaultState);
    function fees() external view returns (Fees memory);
    function timers() external view returns (Timers memory);
    function tank() external view returns (Tank memory);
    function allowances() external view returns (Allowances memory);
    function cash() external view returns (uint256);
    function escrow() external view returns (uint256);
    function mark() external view returns (uint256);
    function lastPayment() external view returns (uint64);
    function idleFloor() external view returns (uint16 bps, uint16 pendingBps, uint64 pendingAt);
    function requestCount() external view returns (uint256);
    function requestAt(uint256 requestId) external view returns (Request memory);
    function valuationCount() external view returns (uint256);
    function valuationAt(uint256 index) external view returns (Valuation memory);
    function claimCount() external view returns (uint256);
    function claimAt(uint256 claimId) external view returns (Claim memory);
    function units() external view returns (uint256 total, uint256 reserve);
    function unitsOf(address holder) external view returns (uint256 units, uint256 claimable);
    function heldFor(address owner) external view returns (uint256);
    function homeStation() external view returns (address);
    function strategyId() external view returns (uint256);
    function bundleHash() external view returns (bytes32);
    function author() external view returns (address);
    function exitBandBps() external view returns (uint16);
    function createdAt() external view returns (uint64);
}
