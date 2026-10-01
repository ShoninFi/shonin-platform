// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IStation} from "../interfaces/IStation.sol";
import {
    Claim,
    ClaimState,
    IVault,
    Request,
    RequestKind,
    Valuation,
    ValuationStatus,
    VaultState
} from "../interfaces/IVault.sol";
import {Layout, VaultStorage} from "./VaultStorage.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// Requests, valuations and settlement: how money joins and leaves a vault,
/// and what it pays to.
///
/// Every request is priced at the first valuation at or after its `earliest`,
/// and everything one valuation settles is priced at that valuation's
/// snapshot, in the order the requests were made. A deposit buys at the net
/// price; a withdrawal is paid at the exit-side price, less a slice of its own
/// shares for the profit fee they owe and the transaction fee the pool keeps.
/// Each keeps every other share's price where it was, so the order changes
/// nobody's price.
library Valuations {
    using VaultStorage for Layout;
    using Math for uint256;
    using SafeERC20 for IERC20;

    uint256 private constant OFFSET = VaultStorage.OFFSET;
    uint256 private constant PRICE = VaultStorage.PRICE;
    uint256 private constant BPS = VaultStorage.BPS;
    uint256 private constant WAD = VaultStorage.WAD;
    uint256 private constant YEAR = VaultStorage.YEAR;
    uint256 private constant UNIT_SCALE = VaultStorage.UNIT_SCALE;

    // Requests --------------------------------------------------------------

    function requestDeposit(uint256 assets, address receiver) external returns (uint256) {
        Layout storage s = VaultStorage.layout();
        if (s.state != VaultState.Open) revert IVault.WrongState();
        if (assets < s.minimumDeposit || receiver == address(0)) revert IVault.TooSmall();
        // Every holder is someone who signed for the vault's list as it is.
        if (s.statementOf[receiver] != s.restrictedHash) revert IVault.NoStatement();
        IERC20(s.asset).safeTransferFrom(msg.sender, address(this), assets);
        s.escrow += assets;
        return s.queue(receiver, RequestKind.Deposit, assets, 0);
    }

    function requestRedeem(uint256 shares, address receiver) external returns (uint256) {
        Layout storage s = VaultStorage.layout();
        VaultState state = s.state;
        if (state == VaultState.None || state == VaultState.Closed || state == VaultState.Cancelled) {
            revert IVault.WrongState();
        }
        if (shares == 0 || receiver == address(0)) revert IVault.TooSmall();
        s.move(msg.sender, address(this), shares);
        s.pendingShares += shares;
        return s.queue(receiver, RequestKind.Redeem, shares, 0);
    }

    // Valuations ------------------------------------------------------------

    function recordValuation() external {
        Layout storage s = VaultStorage.layout();
        VaultState state = s.state;
        if (
            state == VaultState.None || state == VaultState.Activating || state == VaultState.Closed
                || state == VaultState.Cancelled
        ) revert IVault.WrongState();

        uint256 interval = s.timers.slotInterval;
        uint64 time = uint64(block.timestamp - (block.timestamp % interval));
        if (block.timestamp - time > s.timers.slotWindow) revert IVault.OutsideWindow();
        uint256 count = s.valuations.length;
        if (count != 0) {
            Valuation storage previous = s.valuations[count - 1];
            if (previous.time >= time) revert IVault.AlreadyValued();
            if (previous.status == ValuationStatus.Open) revert IVault.Unsettled();
        }
        if (owed(s)) revert IVault.Unsettled();

        syncHolding(s);
        (uint256 markValue, uint256 exitValue) = IStation(s.homeStation).value();
        if (s.units != 0) {
            // The stations' value belongs partly to the fallback's unit
            // holders, who hold it outside the shares.
            markValue = markValue.mulDiv(WAD - s.units, WAD);
            exitValue = exitValue.mulDiv(WAD - s.units, WAD);
        }
        uint32 missing = uint32(s.otherChains.length);
        s.valuations.push(
            Valuation({
                time: time,
                status: ValuationStatus.Open,
                gross: s.cash + markValue + 1,
                grossExit: s.cash + exitValue + 1,
                supply: 0,
                fee: 0,
                feeExit: 0,
                cash: s.cash,
                postsMissing: missing
            })
        );
        if (missing == 0) {
            price(s, count);
        } else {
            emit IVault.ValuationOpened(count, time, missing);
        }
    }

    /// Fix the valuation's prices: run payday if it is due, then snapshot.
    function price(Layout storage s, uint256 index) internal {
        Valuation storage v = s.valuations[index];
        uint256 gross = v.gross;
        // An exit is never worth more than the mark: a bid above it would pay
        // a leaver more than the pool holds for them.
        if (v.grossExit > gross) v.grossExit = gross;
        uint256 supply = s.totalSupply + OFFSET;
        if (s.state != VaultState.Closed && v.time >= VaultStorage.nextMonday(s.lastPayment)) {
            supply = payday(s, gross, supply, v.time);
        }
        v.supply = supply;
        v.fee = s.feeOf(gross, supply);
        v.feeExit = s.feeOf(v.grossExit, supply);
        v.status = ValuationStatus.Priced;
        s.lastPriced = index + 1;
        emit IVault.Valued(index, v.time, gross, v.grossExit, supply, v.fee);
    }

    /// Pay the profit fee owed, in new shares, and hand the holding fee
    /// over. The price is the same after as before.
    function payday(
        Layout storage s,
        uint256 gross,
        uint256 supply,
        uint64 time
    ) internal returns (uint256) {
        uint256 fee = s.feeOf(gross, supply);
        uint256 minted = fee == 0 ? 0 : fee.mulDiv(supply, gross - fee);
        uint256 total = uint256(s.fees.authorBps) + s.fees.platformBps;
        uint256 toAuthor = total == 0 ? 0 : minted.mulDiv(s.fees.authorBps, total);
        s.mint(s.author, toAuthor);
        s.mint(s.platform, minted - toAuthor);
        uint256 holding = s.unpaidHolding;
        if (holding != 0) {
            s.move(address(this), s.platform, holding);
            s.unpaidHolding = 0;
        }
        supply += minted;
        uint256 raised = gross.mulDiv(PRICE, supply, Math.Rounding.Ceil);
        if (raised > s.mark) s.mark = raised;
        s.lastPayment = time;
        emit IVault.FeesPaid(toAuthor, minted - toAuthor, holding, s.mark);
        return supply;
    }

    /// Mint the holding fee for the time since the last sync to the vault.
    /// Minting leaves the mark alone, so it lowers value per share against
    /// the mark, as a holding fee should.
    function syncHolding(Layout storage s) internal {
        uint256 elapsed = block.timestamp - s.lastHolding;
        if (elapsed == 0) return;
        s.lastHolding = uint64(block.timestamp);
        uint256 rate = uint256(s.fees.holdingBps) * elapsed * WAD / (BPS * YEAR);
        if (rate == 0 || rate >= WAD) return;
        uint256 minted = (s.totalSupply + OFFSET).mulDiv(rate, WAD - rate);
        s.mint(address(this), minted);
        s.unpaidHolding += minted;
        emit IVault.HoldingSynced(minted);
    }

    /// Whether a priced valuation still has requests waiting on it. A new
    /// valuation waits until they are settled, so each prices only its own.
    function owed(Layout storage s) internal view returns (bool) {
        if (s.lastPriced == 0) return false;
        uint256 i = s.requestCursor;
        uint256 count = s.requests.length;
        while (i < count && s.requests[i].done) i++;
        if (i == count) return false;
        return s.requests[i].earliest <= s.latest().time;
    }

    // Settlement ------------------------------------------------------------

    function settle(uint256 max) external returns (uint256 settled) {
        Layout storage s = VaultStorage.layout();
        uint256 i = s.requestCursor;
        uint256 vi = s.valuationCursor;
        uint256 count = s.requests.length;
        while (settled < max && i < count) {
            Request storage r = s.requests[i];
            if (r.done) {
                i++;
                continue;
            }
            // The first priced valuation at or after its earliest. A void one
            // passes its requests on; an open one isn't decided yet.
            bool found;
            while (vi < s.valuations.length) {
                Valuation storage candidate = s.valuations[vi];
                if (candidate.status == ValuationStatus.Open) break;
                if (candidate.status == ValuationStatus.Void || candidate.time < r.earliest) {
                    vi++;
                    continue;
                }
                found = true;
                break;
            }
            if (!found) break;
            r.done = true;
            _settleOne(s, r, i + 1, s.valuations[vi]);
            i++;
            settled++;
        }
        s.requestCursor = i;
        s.valuationCursor = vi;
    }

    function _settleOne(Layout storage s, Request storage r, uint256 id, Valuation storage v) private {
        RequestKind kind = r.kind;
        if (kind == RequestKind.Deposit) {
            _deposit(s, r, id, v);
        } else if (kind == RequestKind.Redeem) {
            _redeem(s, r, id, v);
        } else if (kind == RequestKind.Claim) {
            _claim(s, r, id, v);
        } else {
            _fill(s, r, v);
        }
    }

    function _deposit(Layout storage s, Request storage r, uint256 id, Valuation storage v) private {
        uint256 assets = r.amount;
        s.escrow -= assets;
        // A vault that stopped taking deposits, or a list the depositor
        // hasn't signed for, sends the money back.
        if (s.state != VaultState.Open || s.statementOf[r.owner] != s.restrictedHash) {
            s.pay(r.owner, assets);
            emit IVault.Refunded(id, r.owner, assets);
            return;
        }
        uint256 net = v.gross - v.fee;
        uint256 shares =
            (assets - assets.mulDiv(s.fees.transactionBps, BPS, Math.Rounding.Ceil)).mulDiv(v.supply, net);
        if (shares == 0) {
            s.pay(r.owner, assets);
            emit IVault.Refunded(id, r.owner, assets);
            return;
        }
        // Money joining above the mark joins at its own price: the mark
        // becomes the average, so the fee owed doesn't jump and nobody's price
        // moves. Below the mark, the newcomer rides the recovery fee-free.
        uint256 joined = net.mulDiv(PRICE, v.supply, Math.Rounding.Ceil);
        if (joined > s.mark) {
            uint256 live = s.totalSupply + OFFSET;
            s.mark = (s.mark * live + joined * shares).ceilDiv(live + shares);
            emit IVault.MarkRaised(s.mark);
        }
        s.cash += assets;
        s.mint(r.owner, shares);
        emit IVault.Deposited(id, r.owner, assets, shares);
    }

    /// What `shares` come to at this valuation's exit price: the slice `k` of
    /// them that pays the profit fee they owe, the rest `b` burned, and the
    /// payout after the transaction fee.
    function exitOf(
        Layout storage s,
        Valuation storage v,
        uint256 shares
    ) internal view returns (uint256 slice, uint256 burned, uint256 payout) {
        uint256 grossExit = v.grossExit;
        slice = v.feeExit == 0 ? 0 : shares.mulDiv(v.feeExit, grossExit, Math.Rounding.Ceil);
        burned = shares - slice;
        uint256 gross = burned.mulDiv(grossExit, v.supply);
        payout = gross - gross.mulDiv(s.fees.transactionBps, BPS, Math.Rounding.Ceil);
    }

    /// Hand the fee slice to the author and the platform, and burn the rest.
    function _distribute(Layout storage s, address from, uint256 slice, uint256 burned) private {
        if (slice != 0) {
            uint256 total = uint256(s.fees.authorBps) + s.fees.platformBps;
            uint256 toAuthor = slice.mulDiv(s.fees.authorBps, total);
            s.move(from, s.author, toAuthor);
            s.move(from, s.platform, slice - toAuthor);
        }
        s.burn(from, burned);
    }

    function _redeem(Layout storage s, Request storage r, uint256 id, Valuation storage v) private {
        uint256 shares = r.amount;
        (uint256 slice, uint256 burned, uint256 payout) = exitOf(s, v, shares);
        if (payout <= s.cash) {
            s.pendingShares -= shares;
            _distribute(s, address(this), slice, burned);
            s.cash -= payout;
            s.pay(r.owner, payout);
            emit IVault.Withdrawn(id, r.owner, shares, payout, slice);
            return;
        }
        // The cash can't pay it: a claim, its shares still pending and still
        // outstanding, so the leaver bears the close with everyone else.
        s.claims.push(
            Claim({
                owner: r.owner,
                shares: shares,
                bookedAt: uint64(block.timestamp),
                lastAttemptAt: 0,
                state: ClaimState.Pending,
                paid: 0
            })
        );
        emit IVault.ClaimBooked(s.claims.length, r.owner, shares);
    }

    function _claim(Layout storage s, Request storage r, uint256 id, Valuation storage v) private {
        Claim storage claim = s.claims[r.ref - 1];
        if (claim.state != ClaimState.Queued) return;
        s.queuedClaims -= 1;
        uint256 shares = claim.shares;
        (uint256 slice, uint256 burned, uint256 payout) = exitOf(s, v, shares);
        if (payout > s.cash) {
            claim.state = ClaimState.Pending;
            emit IVault.ClaimTried(r.ref, true);
            return;
        }
        s.pendingShares -= shares;
        _distribute(s, address(this), slice, burned);
        s.cash -= payout;
        claim.state = ClaimState.Paid;
        claim.paid = payout;
        s.pay(claim.owner, payout);
        emit IVault.Withdrawn(id, claim.owner, shares, payout, slice);
        emit IVault.ClaimPaid(r.ref, payout);
    }

    /// The author's shares, redeemed into the tank like any withdrawal, from
    /// cash only, and never as a claim: what the cash or the shares don't
    /// cover is simply not filled.
    function _fill(Layout storage s, Request storage r, Valuation storage v) private {
        address author = s.author;
        uint256 shares = r.amount;
        if (r.ref == 1) {
            s.tank.fillPending = false;
            shares = _sharesFor(s, v, r.amount);
        }
        uint256 held = s.balanceOf[author];
        if (shares > held) shares = held;
        if (shares == 0) return;
        (uint256 slice, uint256 burned, uint256 payout) = exitOf(s, v, shares);
        if (payout > s.cash) {
            shares = shares.mulDiv(s.cash, payout);
            (slice, burned, payout) = exitOf(s, v, shares);
            // The rescaled figure rounds on its own and can still land a unit
            // over; the shares then buy a unit less, never more cash than is
            // there.
            if (payout > s.cash) payout = s.cash;
        }
        if (payout == 0) return;
        _distribute(s, author, slice, burned);
        s.cash -= payout;
        s.tank.balance += payout;
        emit IVault.TankFilled(shares, payout);
    }

    /// How many shares pay `assets` at this valuation's exit price, after the
    /// fee slice and the transaction fee.
    function _sharesFor(
        Layout storage s,
        Valuation storage v,
        uint256 assets
    ) private view returns (uint256) {
        uint256 perShare = v.grossExit - v.feeExit;
        uint256 keep = BPS - s.fees.transactionBps;
        if (perShare == 0 || keep == 0) return 0;
        return assets.mulDiv(v.supply * BPS, perShare * keep, Math.Rounding.Ceil);
    }

    // The pro-rata fallback -------------------------------------------------

    /// A request nothing has priced for `fallbackAfter`. A redemption takes
    /// the cash's share now and holds units for the rest of what the stations
    /// hold; a deposit, whose money never left, takes it back.
    function takeProRata(uint256 requestId) external {
        Layout storage s = VaultStorage.layout();
        Request storage r = s.requests[requestId - 1];
        bool deposit = r.kind == RequestKind.Deposit;
        if ((!deposit && r.kind != RequestKind.Redeem) || r.done || msg.sender != r.owner) {
            revert IVault.NotAllowed();
        }
        if (block.timestamp < r.earliest + s.timers.fallbackAfter) revert IVault.NotYet();
        // Not while a valuation could price it: the fallback is for when
        // nothing can, not a way round the fee.
        if (s.lastPriced != 0 && s.latest().time >= r.earliest) revert IVault.NotAllowed();
        r.done = true;
        if (deposit) {
            s.escrow -= r.amount;
            s.pay(r.owner, r.amount);
            emit IVault.Refunded(requestId, r.owner, r.amount);
            return;
        }
        s.pendingShares -= r.amount;
        _proRata(s, r.owner, r.amount);
    }

    function takeProRataClaim(uint256 claimId) external {
        Layout storage s = VaultStorage.layout();
        Claim storage claim = s.claims[claimId - 1];
        if (msg.sender != claim.owner) revert IVault.NotAllowed();
        if (claim.state != ClaimState.Pending && claim.state != ClaimState.Waiting) {
            revert IVault.NotAllowed();
        }
        uint256 since = s.lastPriced == 0 ? s.createdAt : s.latest().time;
        if (claim.bookedAt > since) since = claim.bookedAt;
        if (block.timestamp < since + s.timers.fallbackAfter) revert IVault.NotYet();
        claim.state = ClaimState.Fallback;
        s.pendingShares -= claim.shares;
        _proRata(s, claim.owner, claim.shares);
    }

    /// No fee: nothing can be priced. The leaver's units are a claim on
    /// their fraction of whatever the stations hold.
    function _proRata(Layout storage s, address owner, uint256 shares) private {
        uint256 assets = s.cash.mulDiv(shares, s.totalSupply);
        // The shares own what the units don't; these shares' part of that
        // is the leaver's for good, whatever fees are minted later.
        uint256 fraction = shares.mulDiv(WAD - s.units, s.totalSupply);
        s.burn(address(this), shares);
        s.cash -= assets;
        if (fraction != 0) {
            _accrue(s, owner);
            s.unitsOf[owner] += fraction;
            s.units += fraction;
            s.unitDebt[owner] = s.unitsOf[owner].mulDiv(s.perUnit, UNIT_SCALE);
        }
        s.pay(owner, assets);
        emit IVault.ProRataTaken(owner, shares, assets, fraction);
    }

    function _accrue(Layout storage s, address holder) private {
        uint256 earned = s.unitsOf[holder].mulDiv(s.perUnit, UNIT_SCALE);
        s.unitCredit[holder] += earned - s.unitDebt[holder];
        s.unitDebt[holder] = earned;
    }

    function collectUnits() external returns (uint256 assets) {
        Layout storage s = VaultStorage.layout();
        _accrue(s, msg.sender);
        assets = s.unitCredit[msg.sender];
        if (assets == 0) revert IVault.Nothing();
        s.unitCredit[msg.sender] = 0;
        s.unitReserve -= assets;
        s.pay(msg.sender, assets);
        emit IVault.UnitsPaid(msg.sender, assets);
    }

    /// Close exactly one holder's fraction of every position and give them
    /// all of what it brings home. What remains is owned by everyone else in
    /// the same proportions as before.
    function serveProRata(address holder) external {
        Layout storage s = VaultStorage.layout();
        uint256 fraction = s.unitsOf[holder];
        if (fraction == 0) revert IVault.Nothing();
        s.serving = holder;
        (, bool blocked) = IStation(s.homeStation).unwind(fraction);
        s.serving = address(0);
        if (blocked) revert IVault.NotYet();
        _accrue(s, holder);
        s.units -= fraction;
        s.unitsOf[holder] = 0;
        s.unitDebt[holder] = 0;
    }

    // The home station ------------------------------------------------------

    function toStation(uint256 amount) external {
        Layout storage s = VaultStorage.layout();
        if (msg.sender != s.homeStation) revert IVault.NotAllowed();
        // Nothing goes out while the vault is stopping, while unit holders
        // are owed part of what is out (sending more would need a price), or
        // while a claim's cash is home waiting to be priced: taking it back
        // out would leave the claim unpaid.
        if (!s.live() || s.halted() || s.units != 0 || s.queuedClaims != 0) revert IVault.WrongState();
        // The stations as last valued, and the cash as it is now, so money
        // that joined or left since the snapshot counts.
        uint256 value = s.cash + 1;
        if (s.lastPriced != 0) {
            Valuation storage v = s.latest();
            value += v.gross - 1 - v.cash;
        }
        uint256 floor = value.mulDiv(floorNow(s), BPS, Math.Rounding.Ceil);
        if (amount > s.cash || s.cash - amount < floor) revert IVault.BelowFloor();
        s.cash -= amount;
        IERC20(s.asset).safeTransfer(msg.sender, amount);
        emit IVault.ToStation(amount);
    }

    function fromStation(uint256 amount) external {
        Layout storage s = VaultStorage.layout();
        if (msg.sender != s.homeStation) revert IVault.NotAllowed();
        IERC20(s.asset).safeTransferFrom(msg.sender, address(this), amount);
        address serving = s.serving;
        if (serving != address(0)) {
            s.unitCredit[serving] += amount;
            s.unitReserve += amount;
            emit IVault.FromStation(amount, amount);
            return;
        }
        uint256 toUnits;
        if (s.units != 0) {
            toUnits = amount.mulDiv(s.units, WAD);
            s.perUnit += toUnits.mulDiv(UNIT_SCALE, s.units);
            s.unitReserve += toUnits;
        }
        s.cash += amount - toUnits;
        emit IVault.FromStation(amount, toUnits);
    }

    /// The idle floor in force: a lowering applies once its delay has passed.
    function floorNow(Layout storage s) internal view returns (uint256) {
        if (s.pendingFloorAt != 0 && block.timestamp >= s.pendingFloorAt) return s.pendingFloorBps;
        return s.floorBps;
    }
}
