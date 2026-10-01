// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Allowances} from "../interfaces/IPrices.sol";
import {
    Claim,
    Fees,
    IVault,
    Request,
    RequestKind,
    Tank,
    Timers,
    Valuation,
    ValuationStatus,
    VaultState
} from "../interfaces/IVault.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// One station's post for one valuation.
struct Post {
    bool posted;
    uint256 mark;
    uint256 exit;
}

/// Everything a vault holds, in one place, so the vault and the libraries it
/// is split into read and write the same state.
struct Layout {
    // Who it is -------------------------------------------------------------
    address factory;
    address asset;
    address attestation;
    address prices;
    address homeStation;
    address platform;
    address admin;
    address author;
    uint256 strategyId;
    bytes32 bundleHash;
    uint64 createdAt;
    uint16 exitBandBps;
    Fees fees;
    Timers timers;
    uint256 minimumDeposit;
    uint256[] otherChains;
    bytes32[] drivers;
    // Where it is in its life ----------------------------------------------
    VaultState state;
    uint64 haltedAt;
    uint64 retiredAt;
    bytes32 haltReason;
    // Shares ----------------------------------------------------------------
    uint256 totalSupply;
    mapping(address => uint256) balanceOf;
    /// Shares the vault holds for redemptions and claims not yet paid.
    uint256 pendingShares;
    /// Holding-fee shares minted to the vault, handed over on payday.
    uint256 unpaidHolding;
    // Accounting ------------------------------------------------------------
    uint256 cash;
    uint256 escrow;
    /// The high-water mark: `1e18` is $1.00 a share.
    uint256 mark;
    uint64 lastHolding;
    uint64 lastPayment;
    // The tank --------------------------------------------------------------
    Tank tank;
    Allowances allowances;
    uint64 allowancesSince;
    // The idle floor --------------------------------------------------------
    uint16 floorBps;
    uint16 pendingFloorBps;
    uint64 pendingFloorAt;
    // Requests, valuations and claims ---------------------------------------
    Request[] requests;
    uint256 requestCursor;
    Valuation[] valuations;
    uint256 valuationCursor;
    /// The newest priced valuation, plus one; zero before any.
    uint256 lastPriced;
    Claim[] claims;
    /// Claims whose fraction was closed and brought home, waiting to be
    /// priced. Their cash stays at home until they are.
    uint256 queuedClaims;
    /// Cash set aside for those claims, which nothing else may pay from.
    uint256 reservedCash;
    mapping(uint256 => uint256) claimReserved;
    /// The request a queued claim is priced by.
    mapping(uint256 => uint256) claimRequest;
    /// What a queued claim's close brought home, and its share of the cash
    /// as it stood then: together, all its shares own once it is closed.
    mapping(uint256 => uint256) claimHome;
    mapping(uint256 => uint256) claimCash;
    /// The shares of queued claims whose fraction is already home: they own
    /// cash, and none of what the stations still hold.
    uint256 closedShares;
    mapping(uint256 => mapping(uint256 => Post)) posts;
    /// The open valuation's unit fraction and when it was recorded, so the
    /// posts that complete it are counted as of the same moment.
    uint256 openUnits;
    /// How many sends the home station had made when it was valued: the ones
    /// after it are still in its value.
    uint256 sendsAtOpen;
    /// Exit costs an unwinder reported for a claim, and who is paid them.
    uint256 maxExitCost;
    mapping(uint256 => uint256) exitCost;
    mapping(uint256 => address) exitCostTo;
    // The pro-rata fallback -------------------------------------------------
    /// The fallback's units, as raw amounts. A holder's live fraction of the
    /// stations is their raw units times `unitScale`; the scale grows each
    /// time a holder is served, since what is left is then everyone else's
    /// in the same proportions. Fee shares minted later can't dilute them.
    uint256 units;
    /// Zero means `WAD`.
    uint256 unitScale;
    uint256 unitReserve;
    /// What each unit has received, scaled by `UNIT_SCALE`.
    uint256 perUnit;
    mapping(address => uint256) unitsOf;
    mapping(address => uint256) unitDebt;
    mapping(address => uint256) unitCredit;
    /// The holder a `serveProRata` is bringing money home for.
    address serving;
    /// A claim's close is under way: what it brings home is the claim's,
    /// none of it the unit holders'.
    bool closing;
    /// Everything the home station has ever taken, and how much it had taken
    /// when each valuation looked at it, so the idle floor is measured on a
    /// value pulls can't shrink, whatever became of the valuations since.
    uint256 sentTotal;
    mapping(uint256 => uint256) sentAtValuation;
    // Who may deposit -------------------------------------------------------
    bytes32[] restricted;
    bytes32 restrictedHash;
    mapping(address => bytes32) statementOf;
    // Payouts a receiver refused --------------------------------------------
    mapping(address => uint256) held;
    uint256 heldTotal;
}

/// The vault's storage, and the bookkeeping every part of it shares.
library VaultStorage {
    using Math for uint256;

    /// keccak256(abi.encode(uint256(keccak256("shonin.vault")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant SLOT = 0x34ea905cd715cbd84757816ad68fe9b0e3ec6c9dcb069eb0b53e1a3122faeb00;

    /// Virtual shares and one virtual asset unit, so a share starts at $1 and
    /// the first deposit can't be rounded away.
    uint256 internal constant OFFSET = 1e12;
    /// `s` share units at mark `m` are worth `s * m / PRICE` asset units.
    uint256 internal constant PRICE = 1e30;
    uint256 internal constant BPS = 10_000;
    uint256 internal constant WAD = 1e18;
    uint256 internal constant YEAR = 365 days;
    uint256 internal constant UNIT_SCALE = 1e30;
    /// Where the dead shares minted at activation live.
    address internal constant DEAD = 0x000000000000000000000000000000000000dEaD;

    function layout() internal pure returns (Layout storage s) {
        bytes32 slot = SLOT;
        assembly {
            s.slot := slot
        }
    }

    // Shares ----------------------------------------------------------------

    function mint(Layout storage s, address to, uint256 amount) internal {
        if (amount == 0) return;
        s.totalSupply += amount;
        s.balanceOf[to] += amount;
        emit IVault.Transfer(address(0), to, amount);
    }

    function burn(Layout storage s, address from, uint256 amount) internal {
        if (amount == 0) return;
        s.balanceOf[from] -= amount;
        s.totalSupply -= amount;
        emit IVault.Transfer(from, address(0), amount);
    }

    function move(Layout storage s, address from, address to, uint256 amount) internal {
        if (amount == 0) return;
        s.balanceOf[from] -= amount;
        s.balanceOf[to] += amount;
        emit IVault.Transfer(from, to, amount);
    }

    // Paying out ------------------------------------------------------------

    /// Pay `amount` to `to`, or, if the dollar refuses that address, hold it
    /// for them to collect. One receiver can never stop the queue.
    function pay(Layout storage s, address to, uint256 amount) internal {
        if (amount == 0) return;
        (bool ok, bytes memory answer) = s.asset.call(abi.encodeCall(IERC20.transfer, (to, amount)));
        if (ok && (answer.length == 0 || abi.decode(answer, (bool)))) return;
        s.held[to] += amount;
        s.heldTotal += amount;
        emit IVault.Held(to, amount);
    }

    // Prices ----------------------------------------------------------------

    /// The profit fee owed on `gross` with `supply` shares: a share of what
    /// the vault is worth above the high-water mark, never below zero.
    function feeOf(Layout storage s, uint256 gross, uint256 supply) internal view returns (uint256) {
        uint256 basis = s.mark.mulDiv(supply, PRICE, Math.Rounding.Ceil);
        if (gross <= basis) return 0;
        return (gross - basis).mulDiv(uint256(s.fees.authorBps) + s.fees.platformBps, BPS);
    }

    /// The first slot boundary at least `slotLead` from now: the earliest a
    /// request made now can be priced.
    function nextSlot(Layout storage s) internal view returns (uint64) {
        uint256 interval = s.timers.slotInterval;
        uint256 t = block.timestamp + s.timers.slotLead;
        return uint64(t.ceilDiv(interval) * interval);
    }

    /// Monday 00:00 UTC of the week after the one `t` falls in.
    function nextMonday(uint256 t) internal pure returns (uint256) {
        uint256 day = t / 1 days;
        // The first of January 1970 was a Thursday, three days after a Monday.
        uint256 monday = (day - ((day + 3) % 7)) * 1 days;
        return monday + 7 days;
    }

    function live(Layout storage s) internal view returns (bool) {
        return s.state == VaultState.Open || s.state == VaultState.WindingDown;
    }

    function halted(Layout storage s) internal view returns (bool) {
        return s.haltedAt != 0;
    }

    /// The fraction of the stations, scaled by `WAD`, the unit holders own.
    function liveUnits(Layout storage s) internal view returns (uint256) {
        return s.units == 0 ? 0 : s.units.mulDiv(scale(s), WAD);
    }

    function scale(Layout storage s) internal view returns (uint256) {
        return s.unitScale == 0 ? WAD : s.unitScale;
    }

    /// Cash nothing has set aside: what withdrawals, fills and the station
    /// may draw on.
    function free(Layout storage s) internal view returns (uint256) {
        return s.cash - s.reservedCash;
    }

    function latest(Layout storage s) internal view returns (Valuation storage) {
        return s.valuations[s.lastPriced - 1];
    }

    /// Halted, and a valuation since has found the stations empty: nothing
    /// is left to price, and the vault only waits to close. No new request
    /// is taken, so nobody can hold the close off a slot at a time; holders
    /// redeem once it has closed, fee-free.
    /// A valuation is waiting for other chains' posts: until it is closed,
    /// nothing may change the supply or the units it will be priced on.
    function valuing(Layout storage s) internal view returns (bool) {
        uint256 count = s.valuations.length;
        return count != 0 && s.valuations[count - 1].status == ValuationStatus.Open;
    }

    function readyToClose(Layout storage s) internal view returns (bool) {
        if (!halted(s) || s.lastPriced == 0) return false;
        Valuation storage v = latest(s);
        return v.time > s.haltedAt && v.gross == v.cash + 1;
    }

    function queue(
        Layout storage s,
        address owner,
        RequestKind kind,
        uint256 amount,
        uint256 ref
    ) internal returns (uint256 id) {
        uint64 earliest = nextSlot(s);
        s.requests.push(
            Request({owner: owner, kind: kind, earliest: earliest, done: false, amount: amount, ref: ref})
        );
        id = s.requests.length;
        emit IVault.Requested(id, owner, kind, earliest, amount);
    }
}
