// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IStation} from "../interfaces/IStation.sol";
import {Claim, ClaimState, IVault, Request, RequestKind, VaultState} from "../interfaces/IVault.sol";
import {Valuations} from "./Valuations.sol";
import {Layout, VaultStorage} from "./VaultStorage.sol";
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
library Claims {
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

        // The holding fee up to now falls on every share, the leaver's too.
        Valuations.syncHolding(s);
        uint256 supply = s.totalSupply + VaultStorage.OFFSET;
        // The leaver's part of what the shares own of the stations, and of
        // the cash as it stands before the close.
        uint256 fraction = shares.mulDiv(WAD - s.liveUnits(), supply);
        if (fraction > WAD) fraction = WAD;
        if (fraction == 0) fraction = 1;
        uint256 cashShare = s.cash.mulDiv(shares, supply);

        (uint256 home, bool blocked) = _close(s, fraction);
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
            // The close came from the shares' part alone: the unit holders
            // own the same as before, now a larger fraction of what is left.
            if (s.units != 0 && fraction < WAD) s.unitScale = s.scale().mulDiv(WAD, WAD - fraction);
            _pay(s, claim, claimId, home + cashShare);
        }
        emit IVault.ClaimTried(claimId, blocked);
    }

    /// Close `fraction` at the home station. Proceeds a venue keeps as margin
    /// leave the close undone, so a claim that can't come home sells nothing
    /// however often it is tried.
    function _close(Layout storage s, uint256 fraction) private returns (uint256 home, bool blocked) {
        s.closing = true;
        try IStation(s.homeStation).unwind(fraction) returns (uint256 sent, bool outside) {
            (home, blocked) = (sent, outside);
        } catch (bytes memory reason) {
            if (bytes4(reason) != IStation.ShortSweep.selector) {
                assembly ("memory-safe") {
                    revert(add(reason, 32), mload(reason))
                }
            }
        }
        s.closing = false;
    }

    /// Pay a claim what its shares own, less its own profit fee, paid in
    /// cash to the author and the platform, and the transaction fee, which
    /// stays in the pool. The profit fee is measured against the mark as it
    /// is now, which is what the stayers' fee is measured against too.
    function _pay(Layout storage s, Claim storage claim, uint256 claimId, uint256 owned) private {
        if (owned > s.cash) owned = s.cash;
        uint256 shares = claim.shares;
        uint256 fee = s.feeOf(owned, shares);
        uint256 gross = owned - fee;
        uint256 kept = gross.mulDiv(s.fees.transactionBps, BPS, Math.Rounding.Ceil);
        uint256 payout = gross - kept;

        s.pendingShares -= shares;
        s.burn(address(this), shares);
        s.cash -= owned - kept;
        claim.state = ClaimState.Paid;
        if (fee != 0) {
            uint256 total = uint256(s.fees.authorBps) + s.fees.platformBps;
            uint256 toAuthor = fee.mulDiv(s.fees.authorBps, total);
            s.pay(s.author, toAuthor);
            s.pay(s.platform, fee - toAuthor);
        }
        payout -= Valuations.payExitCost(s, claimId, payout);
        claim.paid = payout;
        s.pay(claim.owner, payout);
        emit IVault.Withdrawn(0, claim.owner, shares, payout, 0);
        emit IVault.ClaimPaid(claimId, payout);
    }

    /// A claim nothing has priced for `fallbackAfter` takes the pro-rata
    /// fallback: its share of the cash now, and units for the rest.
    function takeProRataClaim(uint256 claimId) external {
        Layout storage s = VaultStorage.layout();
        Claim storage claim = s.claims[claimId - 1];
        if (msg.sender != claim.owner) revert IVault.NotAllowed();
        ClaimState state = claim.state;
        if (state != ClaimState.Pending && state != ClaimState.Waiting && state != ClaimState.Queued) {
            revert IVault.NotAllowed();
        }
        uint256 since = s.lastPriced == 0 ? s.createdAt : s.latest().time;
        if (claim.bookedAt > since) since = claim.bookedAt;
        if (block.timestamp < since + s.timers.fallbackAfter || s.valuing()) revert IVault.NotYet();
        if (state == ClaimState.Queued) {
            // Not while a priced valuation could settle it.
            Request storage r = s.requests[s.claimRequest[claimId] - 1];
            if (s.lastPriced != 0 && s.latest().time >= r.earliest) revert IVault.NotAllowed();
            s.queuedClaims -= 1;
            r.done = true;
            s.passDone();
        }
        claim.state = ClaimState.Fallback;
        s.pendingShares -= claim.shares;
        Valuations.proRata(s, claim.owner, claim.shares);
    }
}
