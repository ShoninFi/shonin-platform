// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IStation} from "../interfaces/IStation.sol";
import {
    Claim, ClaimState, IVault, Request, RequestKind, Valuation, VaultState
} from "../interfaces/IVault.sol";
import {Valuations} from "./Valuations.sol";
import {Layout, VaultStorage} from "./VaultStorage.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// Withdrawals the cash couldn't pay.
///
/// A claim is served by closing the leaver's fraction of every position and
/// bringing it home, and it is paid in the same transaction: what the close
/// brought home and the leaver's part of the cash as it stood just before,
/// less their own profit fee and the transaction fee. Its shares are burned
/// there and then, so nothing else in the vault ever has to account for a
/// leaver who is half gone. A claim the cash already covers needs no close:
/// it becomes an ordinary request, priced at the next valuation.
///
/// Where the home station's close settles after the block, the close is
/// staged for the claim, which is closing until it is delivered: it is paid
/// then, from what its own close fetched and its part of the cash fixed and
/// set aside at the stage, its shares burned then. Nothing is priced or
/// divided meanwhile, and no other claim or serve is tried.
library Claims {
    using SafeERC20 for IERC20;
    using VaultStorage for Layout;
    using Math for uint256;

    uint256 private constant BPS = VaultStorage.BPS;
    uint256 private constant WAD = VaultStorage.WAD;

    /// Anyone may try a claim. Trying it again once it is queued is
    /// refused, so nobody can make the vault close more than the leaver's
    /// share.
    function settleClaim(uint256 claimId) external {
        Layout storage s = VaultStorage.layout();
        Claim storage claim = s.claims[claimId - 1];
        if (claim.state != ClaimState.Pending && claim.state != ClaimState.Waiting) {
            revert IVault.NotAllowed();
        }
        // A vault ready to close pays its claims from the closed vault; and
        // nothing may change the supply while a valuation waits for posts.
        if (
            s.state == VaultState.Closed || s.state == VaultState.Cancelled || s.readyToClose() || s.valuing()
        ) revert IVault.WrongState();
        // One close at a time, and a valuation between two of them.
        if (s.staging() || s.valueFirst) revert IVault.NotYet();
        uint256 shares = claim.shares;

        // Cash at home that already covers it: an ordinary request, priced at
        // the next valuation, with nothing to close.
        if (s.lastPriced != 0) {
            (,, uint256 covered) = Valuations.exitOf(s, s.latest(), shares);
            if (covered <= s.cash) {
                claim.state = ClaimState.Queued;
                claim.lastAttemptAt = uint64(block.timestamp);
                s.queuedClaims += 1;
                s.claimRequest[claimId] = s.queue(claim.owner, RequestKind.Claim, shares, claimId);
                emit IVault.ClaimTried(claimId, false);
                return;
            }
        }

        // Cash a priced withdrawal is to be paid from is not for a close's
        // cash share: those are settled first.
        if (Valuations.owed(s)) revert IVault.NotYet();

        // On a vault that trades on other chains a close here reaches only
        // this chain's station, while the leaver owns a share of every one:
        // the claim waits until the cash at home covers it.
        if (s.otherChains.length != 0) {
            claim.state = ClaimState.Waiting;
            emit IVault.ClaimTried(claimId, true);
            return;
        }
        // The holding fee up to now falls on every share, the leaver's too.
        Valuations.syncHolding(s);
        uint256 supply = s.totalSupply + VaultStorage.OFFSET;
        // The leaver's part of what the shares own of the stations, and of
        // the cash as it stands before the close.
        uint256 fraction = shares.mulDiv(WAD - s.liveUnits(), supply);
        if (fraction > WAD) fraction = WAD;
        if (fraction == 0) fraction = 1;
        uint256 cashShare = s.cash.mulDiv(shares, supply);

        // What the stations hold, at the mark now and as the last priced
        // valuation counted them, whichever is more: a bid pushed for one
        // block can't make a claim look like dust. A station that can't be
        // valued is never judged to hold dust.
        uint256 stationsBefore = type(uint256).max / WAD;
        try IStation(s.homeStation).value() returns (uint256 mark, uint256) {
            stationsBefore = mark;
            if (s.lastPriced != 0) {
                Valuation storage last = s.latest();
                uint256 counted = last.gross - 1 - last.cash;
                if (counted > stationsBefore) stationsBefore = counted;
            }
        } catch {}
        (uint256 home, bool blocked, bool failed, bool staged) = _close(s, fraction);
        if (staged) {
            // Paid when its close is delivered: its part of the cash is fixed
            // now and set aside, so nothing else can take it.
            claim.state = ClaimState.Closing;
            claim.lastAttemptAt = uint64(block.timestamp);
            s.stagedClaim = claimId;
            s.stagedFraction = fraction;
            s.stagedCash = cashShare;
            s.cash -= cashShare;
            emit IVault.ClaimTried(claimId, false);
            return;
        }
        // A claim worth fewer dollar-units at the stations than they have
        // parts brings nothing home however often it is closed: it is dust,
        // paid its cash share and done, so it can't hold a vault open or
        // freeze it. Only a close that went through can show that; one that
        // failed proves nothing.
        if (!failed && !blocked && home == 0 && stationsBefore.mulDiv(fraction, WAD) <= s.dust()) {
            _pay(s, claim, claimId, cashShare, claim.shares);
            emit IVault.ClaimTried(claimId, false);
            return;
        }
        if (blocked) {
            // A market outside the band is a real try: the clock restarts.
            claim.state = ClaimState.Waiting;
            claim.lastAttemptAt = uint64(block.timestamp);
        } else if (home == 0 && !failed) {
            // A close went through and brought nothing home for a claim worth
            // more than dust: whatever it sold must not stand.
            revert IVault.NotYet();
        } else if (home == 0) {
            // A close that brought nothing home served nobody, so it doesn't
            // count as a try, and the freeze clock keeps running.
            claim.state = ClaimState.Waiting;
            blocked = true;
        } else {
            // The close came from the shares' part alone: the unit holders
            // own the same as before, now a larger fraction of what is left.
            if (s.units != 0 && fraction < WAD) s.unitScale = s.scale().mulDiv(WAD, WAD - fraction);
            _pay(s, claim, claimId, home + cashShare, claim.shares);
        }
        emit IVault.ClaimTried(claimId, blocked);
    }

    /// Close `fraction` at the home station. A close that fails, whether
    /// proceeds a venue keeps as margin leave it undone or the venue refuses
    /// it outright, sells nothing and brings nothing home: it is not a try,
    /// and the claim waits as it was, however often it is tried.
    function _close(
        Layout storage s,
        uint256 fraction
    ) private returns (uint256 home, bool blocked, bool failed, bool staged) {
        s.closing = true;
        try IStation(s.homeStation).stageClose(fraction) returns (uint256 sent, bool outside, bool placed) {
            (home, blocked, staged) = (sent, outside, placed);
        } catch {
            failed = true;
        }
        s.closing = false;
    }

    /// The home station delivers the close it staged, in whole or in part
    /// (`part`, 1e18 for all): what the delivered part fetched with that
    /// part of the station's idle dollars. A claim is paid that and the same
    /// part of its cash set aside at the stage, and the same part of its
    /// shares, rounded up, is burned; the rest of its cash goes back to the
    /// cash, and it waits with its remaining shares, booked as before. A
    /// holder is credited it, and the same part of their units retired.
    /// `cost`, a loss the close's own sale made, is taken first from what
    /// is paid, the rest as more of the remaining shares or units, at the
    /// rate their stake was worth at the stage (`stake`, plus the claim's
    /// cash), rounded up, never more than are left. Nobody else takes any of
    /// it.
    function delivered(uint256 amount, uint256 part, uint256 cost, uint256 stake) external {
        Layout storage s = VaultStorage.layout();
        if (msg.sender != s.homeStation || !s.staging()) revert IVault.NotAllowed();
        IERC20(s.asset).safeTransferFrom(msg.sender, address(this), amount);
        uint256 claimId = s.stagedClaim;
        uint256 fraction = s.stagedFraction;
        address holder = s.stagedHolder;
        uint256 cashShare = s.stagedCash;
        _unstage(s);
        if (part > WAD) part = WAD;
        // The stations paid out `fraction` x `part` of themselves for whom it
        // was staged alone: the unit holders own the same as before, now a
        // larger fraction of what is left.
        uint256 served = fraction.mulDiv(part, WAD);
        if (claimId != 0) {
            s.cash += amount + cashShare;
            if (s.units != 0 && served != 0 && served < WAD) {
                s.unitScale = s.scale().mulDiv(WAD, WAD - served);
            }
            Claim storage claim = s.claims[claimId - 1];
            uint256 shares = claim.shares;
            uint256 owned = amount + cashShare.mulDiv(part, WAD);
            uint256 burned = shares.mulDiv(part, WAD, Math.Rounding.Ceil);
            (owned, burned) = _charge(owned, burned, shares, cost, stake + cashShare);
            if (burned == 0) {
                // Nothing of it closed or came home: it waits as it was.
                claim.state = ClaimState.Waiting;
                return;
            }
            _pay(s, claim, claimId, owned, burned);
        } else {
            uint256 held = s.unitsOf[holder];
            uint256 gone = held.mulDiv(part, WAD, Math.Rounding.Ceil);
            uint256 credit;
            (credit, gone) = _charge(amount, gone, held, cost, stake);
            // What the cost took from the delivery stays with everyone else.
            s.cash += amount - credit;
            s.retireUnits(holder, served, gone);
            s.unitCredit[holder] += credit;
            s.unitReserve += credit;
            emit IVault.FromStation(amount, credit);
        }
    }

    /// Take `cost` from `paid` first, then as more of what is left of
    /// `whole` beyond `gone`, at `stake` for all of `whole`, rounded up.
    function _charge(
        uint256 paid,
        uint256 gone,
        uint256 whole,
        uint256 cost,
        uint256 stake
    ) private pure returns (uint256, uint256) {
        if (gone > whole) gone = whole;
        uint256 taken = Math.min(cost, paid);
        paid -= taken;
        cost -= taken;
        if (cost != 0) {
            uint256 more = stake == 0 ? whole : cost.mulDiv(whole, stake, Math.Rounding.Ceil);
            gone = Math.min(whole, gone + more);
        }
        return (paid, gone);
    }

    /// The staged close ended without a delivery: the claim waits again,
    /// with its part of the cash back in the cash; units wait as they were.
    function unstaged() external {
        Layout storage s = VaultStorage.layout();
        if (msg.sender != s.homeStation || !s.staging()) revert IVault.NotAllowed();
        uint256 claimId = s.stagedClaim;
        if (claimId != 0) {
            s.claims[claimId - 1].state = ClaimState.Waiting;
            s.cash += s.stagedCash;
        }
        _unstage(s);
    }

    function _unstage(Layout storage s) private {
        s.stagedClaim = 0;
        s.stagedHolder = address(0);
        s.stagedFraction = 0;
        s.stagedCash = 0;
        s.valueFirst = true;
    }

    /// Pay a claim `owned` for `burned` of its shares (all of them, or a
    /// part), less their own profit fee, paid in cash to the author and the
    /// platform, and the transaction fee, which stays in the pool. The
    /// profit fee is measured against the mark as it is now, which is what
    /// the stayers' fee is measured against too. A claim paid for part of
    /// its shares waits again with the rest, booked as before.
    function _pay(
        Layout storage s,
        Claim storage claim,
        uint256 claimId,
        uint256 owned,
        uint256 burned
    ) private {
        if (owned > s.cash) owned = s.cash;
        uint256 fee = s.feeOf(owned, burned);
        uint256 gross = owned - fee;
        uint256 kept = gross.mulDiv(s.fees.transactionBps, BPS, Math.Rounding.Ceil);
        uint256 payout = gross - kept;

        s.pendingShares -= burned;
        s.burn(address(this), burned);
        s.cash -= owned - kept;
        if (fee != 0) {
            uint256 total = uint256(s.fees.authorBps) + s.fees.platformBps;
            uint256 toAuthor = fee.mulDiv(s.fees.authorBps, total);
            s.pay(s.author, toAuthor);
            s.pay(s.platform, fee - toAuthor);
        }
        payout -= Valuations.payExitCost(s, claimId, payout);
        claim.paid += payout;
        s.pay(claim.owner, payout);
        uint256 left = claim.shares - burned;
        emit IVault.Withdrawn(0, claim.owner, burned, payout, 0);
        if (left == 0) {
            claim.state = ClaimState.Paid;
            emit IVault.ClaimPaid(claimId, payout);
        } else {
            claim.shares = left;
            claim.state = ClaimState.Waiting;
            emit IVault.ClaimPartPaid(claimId, burned, payout, left);
        }
    }

    /// A withdrawal nothing has priced for `fallbackAfter` (a market outside
    /// the band voids every valuation, say) may become a claim instead of
    /// taking the fallback: served by a close, and counted toward a freeze
    /// from when it was due to be priced.
    function claimInstead(uint256 requestId) external returns (uint256 claimId) {
        Layout storage s = VaultStorage.layout();
        Request storage r = s.requests[requestId - 1];
        if (r.kind != RequestKind.Redeem || r.done || msg.sender != r.owner) revert IVault.NotAllowed();
        if (s.state == VaultState.Closed || s.state == VaultState.Cancelled || s.readyToClose()) {
            revert IVault.WrongState();
        }
        if (
            block.timestamp < r.earliest + s.timers.fallbackAfter || s.valuing() || Valuations.owed(s)
                || s.staging()
        ) {
            revert IVault.NotYet();
        }
        if (s.lastPriced != 0 && s.latest().time >= r.earliest) revert IVault.NotAllowed();
        r.done = true;
        s.passDone();
        // The shares stay pending, held by the vault, as a claim's are.
        s.claims.push(
            Claim({
                owner: r.owner,
                shares: r.amount,
                bookedAt: r.earliest,
                lastAttemptAt: 0,
                state: ClaimState.Pending,
                paid: 0
            })
        );
        claimId = s.claims.length;
        emit IVault.ClaimBooked(claimId, r.owner, r.amount);
    }

    /// A claim nothing has priced for `fallbackAfter` takes the pro-rata
    /// fallback: its share of the cash now, and units for the rest.
    function takeProRataClaim(uint256 claimId) external {
        Layout storage s = VaultStorage.layout();
        Claim storage claim = s.claims[claimId - 1];
        if (msg.sender != claim.owner) revert IVault.NotAllowed();
        // A vault that is closing or closed pays its claims from the closed
        // vault (`claimClosed`), fees stopped.
        if (s.state == VaultState.Closed || s.state == VaultState.Cancelled || s.readyToClose()) {
            revert IVault.WrongState();
        }
        ClaimState state = claim.state;
        if (state != ClaimState.Pending && state != ClaimState.Waiting && state != ClaimState.Queued) {
            revert IVault.NotAllowed();
        }
        // A day from its booking: a claim nobody could serve for that long
        // (a market outside the band, a close that can't come home) takes
        // its share of the cash and units for the rest, whether or not
        // valuations kept being priced meanwhile.
        if (block.timestamp < claim.bookedAt + s.timers.fallbackAfter || s.valuing() || s.staging()) {
            revert IVault.NotYet();
        }
        // Not while a priced valuation waits to settle, and not for a claim
        // the cash covers: that one can be served now, at a price.
        if (Valuations.owed(s)) revert IVault.NotYet();
        if (state != ClaimState.Queued && s.lastPriced != 0) {
            (,, uint256 covered) = Valuations.exitOf(s, s.latest(), claim.shares);
            if (covered <= s.cash) revert IVault.NotAllowed();
        }
        if (state == ClaimState.Queued) {
            // Not while a priced valuation could settle it.
            Request storage r = s.requests[s.claimRequest[claimId] - 1];
            if (s.lastPriced != 0 && s.latest().time >= r.earliest) revert IVault.NotAllowed();
            // Queued, it is a request like any other, and falls back as one.
            if (block.timestamp < r.earliest + s.timers.fallbackAfter) revert IVault.NotYet();
            s.queuedClaims -= 1;
            r.done = true;
            s.passDone();
        }
        claim.state = ClaimState.Fallback;
        s.pendingShares -= claim.shares;
        Valuations.proRata(s, claim.owner, claim.shares);
    }
}
