// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationRegistry, ImageKind} from "../interfaces/IAttestationRegistry.sol";
import {IStation, Send} from "../interfaces/IStation.sol";
import {
    Claim,
    ClaimState,
    IVault,
    Request,
    RequestKind,
    StationPost,
    Valuation,
    ValuationStatus,
    VaultState
} from "../interfaces/IVault.sol";
import {Layout, Post, VaultStorage} from "./VaultStorage.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";
import {ECDSA} from "openzeppelin/utils/cryptography/ECDSA.sol";
import {MessageHashUtils} from "openzeppelin/utils/cryptography/MessageHashUtils.sol";
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
        uint256 id = s.queue(receiver, RequestKind.Deposit, assets, 0);
        // Sent back, it goes to whoever paid it, not to the receiver named.
        if (msg.sender != receiver) s.payerOf[id] = msg.sender;
        return id;
    }

    function requestRedeem(uint256 shares, address receiver) external returns (uint256) {
        Layout storage s = VaultStorage.layout();
        VaultState state = s.state;
        if (state == VaultState.None || state == VaultState.Closed || state == VaultState.Cancelled) {
            revert IVault.WrongState();
        }
        if (shares == 0 || receiver == address(0)) revert IVault.TooSmall();
        if (s.readyToClose()) revert IVault.WrongState();
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
        // A valuation that waits for other chains' posts is opened by a key
        // blessed for the vault: anyone else could keep one open for ever,
        // and an open valuation holds the fallback shut.
        if (s.otherChains.length != 0 && !_blessed(s, msg.sender, false)) revert IVault.NotBlessed();

        uint256 interval = s.timers.slotInterval;
        uint64 time = uint64(block.timestamp - (block.timestamp % interval));
        if (block.timestamp - time > s.timers.slotWindow) revert IVault.OutsideWindow();
        if (s.otherChains.length != 0 && time < s.closedAt + interval) revert IVault.NotYet();
        uint256 count = s.valuations.length;
        if (count != 0) {
            Valuation storage previous = s.valuations[count - 1];
            if (previous.time >= time) revert IVault.AlreadyValued();
            if (previous.status == ValuationStatus.Open) revert IVault.Unsettled();
        }
        s.passDone();
        if (owed(s)) revert IVault.Unsettled();

        syncHolding(s);
        (uint256 markValue, uint256 exitValue) = IStation(s.homeStation).value();
        // A close in progress at the home station has sold part of what it
        // holds for one leaver, not yet home: such a valuation prices
        // nothing, as one at a market outside the band.
        if (IStation(s.homeStation).outsideBand() || s.staging()) s.wide[count] = true;
        s.valueFirst = false;
        // An exit side above the mark is what the holdings could be sold
        // for: a deposit pays it on top, net of the profit fee a sale there
        // would owe, so it can't buy below the pool's realisable worth; but
        // nobody is paid it, and no withdrawal or fee is measured on it.
        uint256 extra;
        if (exitValue > markValue) (extra, exitValue) = (exitValue - markValue, markValue);
        s.sentAtValuation[count] = s.sentTotal;
        s.openedAt[count] = uint64(block.timestamp);
        s.openedBlock[count] = uint64(block.number);
        uint256 units = s.liveUnits();
        if (units != 0) {
            // The stations' value belongs partly to the fallback's unit
            // holders, who hold it outside the shares.
            markValue = markValue.mulDiv(WAD - units, WAD);
            exitValue = exitValue.mulDiv(WAD - units, WAD);
            extra = extra.mulDiv(WAD - units, WAD);
        }
        s.depositExtra[count] = extra;
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
            _priceOrVoid(s, count);
        } else {
            s.openUnits = units;
            s.sendsAtOpen = IStation(s.homeStation).sendCount();
            emit IVault.ValuationOpened(count, time, missing);
        }
    }

    /// A key blessed for this vault and its bundle on this chain: an
    /// unwinder, or (unless only an unwinder will do) an executor.
    function _blessed(Layout storage s, address signer, bool unwinderOnly) private view returns (bool) {
        IAttestationRegistry registry = IAttestationRegistry(s.attestation);
        if (!registry.isBlessedFor(signer, block.chainid, address(this), s.bundleHash)) return false;
        ImageKind kind = registry.keyInfo(signer).kind;
        return kind == ImageKind.Unwinder || (!unwinderOnly && kind == ImageKind.Executor);
    }

    /// Price a single-chain valuation, or void it when a market was outside
    /// the band: such a valuation prices nothing at all (no deposit, no
    /// withdrawal, no fill, no payday), and its requests roll on.
    function _priceOrVoid(Layout storage s, uint256 index) private {
        if (s.wide[index]) {
            Valuation storage v = s.valuations[index];
            v.status = ValuationStatus.Void;
            emit IVault.ValuationVoided(index, v.time);
            return;
        }
        price(s, index);
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
        s.exitPaid = 0;
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
        // A closed vault settles what is left through its own doors.
        if (s.lastPriced == 0 || s.state == VaultState.Closed) return false;
        uint256 i = s.requestCursor;
        if (i == s.requests.length) return false;
        Request storage r = s.requests[i];
        // One taken through the fallback is passed by `settle`, a bounded
        // number at a time, before the next valuation.
        return r.done || r.earliest <= s.latest().time;
    }

    // Settlement ------------------------------------------------------------

    function settle(uint256 max) external returns (uint256 settled) {
        Layout storage s = VaultStorage.layout();
        if (s.lastPriced == 0) return 0;
        // Every request still owed belongs to the last priced valuation: a
        // new one can't open while any is. Its exits settle first and its
        // deposits after, so each exit meets the pool exactly as the
        // snapshot found it, and each deposit blends the mark into the pool
        // the exits left.
        uint256 index = s.lastPriced - 1;
        uint64 time = s.valuations[index].time;
        uint256 i = s.requestCursor;
        uint256 j = s.exitCursor;
        if (j < i) j = i;
        uint256 count = s.requests.length;
        // Every request passed counts toward `max`, done or not, so no number
        // of requests taken through the fallback can make one call too long.
        uint256 steps;
        // Requests already done (taken through the fallback) are passed
        // whatever their slot, so no order of fallbacks can hold the cursor.
        while (steps < max && i < count && s.requests[i].done) {
            steps++;
            i++;
        }
        if (j < i) j = i;
        while (steps < max && j < count) {
            Request storage r = s.requests[j];
            if (r.earliest > time) break;
            steps++;
            if (!r.done && r.kind != RequestKind.Deposit) {
                r.done = true;
                _settleOne(s, r, j + 1, index);
                settled++;
            }
            j++;
        }
        s.exitCursor = j;
        if (j == count || s.requests[j].earliest > time) {
            // The exits are settled: the slot's deposits pay the price of
            // the pool they left, the gap between what they were paid and
            // the mark included, never the snapshot's.
            if (s.depositsFor != index + 1) {
                uint256 supply = s.totalSupply + OFFSET;
                uint256 gross = s.valuations[index].gross - s.exitPaid + s.depositExtra[index];
                s.depositNet = gross - s.feeOf(gross, supply);
                s.depositSupply = supply;
                s.depositsFor = index + 1;
            }
            while (steps < max && i < j) {
                steps++;
                Request storage r = s.requests[i];
                if (!r.done) {
                    r.done = true;
                    _settleOne(s, r, i + 1, index);
                    settled++;
                }
                i++;
            }
        }
        s.requestCursor = i;
    }

    function _settleOne(Layout storage s, Request storage r, uint256 id, uint256 index) private {
        Valuation storage v = s.valuations[index];
        RequestKind kind = r.kind;
        if (kind == RequestKind.Deposit) {
            _deposit(s, r, id);
        } else if (kind == RequestKind.Redeem) {
            _redeem(s, r, id, v);
        } else if (kind == RequestKind.Claim) {
            _claim(s, r, id, v);
        } else {
            _fill(s, r, v);
        }
    }

    function _refund(Layout storage s, uint256 id, address owner, uint256 assets) private {
        address to = s.refundTo(id, owner);
        s.pay(to, assets);
        emit IVault.Refunded(id, to, assets);
    }

    function _deposit(Layout storage s, Request storage r, uint256 id) private {
        uint256 assets = r.amount;
        s.escrow -= assets;
        // A vault that stopped taking deposits, or a list the depositor
        // hasn't signed for, sends the money back.
        if (s.state != VaultState.Open || s.statementOf[r.owner] != s.restrictedHash) {
            _refund(s, id, r.owner, assets);
            return;
        }
        uint256 net = s.depositNet;
        uint256 supply = s.depositSupply;
        uint256 shares =
            (assets - assets.mulDiv(s.fees.transactionBps, BPS, Math.Rounding.Ceil)).mulDiv(supply, net);
        if (shares == 0) {
            _refund(s, id, r.owner, assets);
            return;
        }
        // Money joining above the mark joins at its own price: the mark
        // becomes the average, so the fee owed doesn't jump and nobody's price
        // moves. Below the mark, the newcomer rides the recovery fee-free.
        uint256 joined = net.mulDiv(PRICE, supply, Math.Rounding.Ceil);
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
        if (payout <= s.free()) {
            s.pendingShares -= shares;
            _distribute(s, address(this), slice, burned);
            s.cash -= payout;
            s.exitPaid += payout;
            s.pay(r.owner, payout);
            emit IVault.Withdrawn(id, r.owner, shares, payout, slice);
            return;
        }
        // The cash can't pay it: a claim, its shares still pending and still
        // outstanding, served by a close inside the band.
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

    /// A claim the cash covered when it was tried, priced like any
    /// withdrawal. If the cash no longer covers it, it waits to be tried
    /// again: nothing was closed for it.
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
        s.exitPaid += payout;
        claim.state = ClaimState.Paid;
        payout -= payExitCost(s, r.ref, payout);
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
        uint256 available = s.free();
        if (payout > available) {
            shares = shares.mulDiv(available, payout);
            (slice, burned, payout) = exitOf(s, v, shares);
            // The rescaled figure rounds on its own and can still land a unit
            // over; the shares then buy a unit less, never more cash than is
            // there.
            if (payout > available) payout = available;
        }
        if (payout == 0) return;
        _distribute(s, author, slice, burned);
        s.cash -= payout;
        s.exitPaid += payout;
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
    /// Whether the home station could be valued now inside the band.
    function _priceable(Layout storage s) private view returns (bool) {
        IStation station = IStation(s.homeStation);
        try station.outsideBand() returns (bool wide) {
            if (wide) return false;
        } catch {
            return false;
        }
        try station.value() returns (uint256, uint256) {
            return true;
        } catch {
            return false;
        }
    }

    function takeProRata(uint256 requestId) external {
        Layout storage s = VaultStorage.layout();
        Request storage r = s.requests[requestId - 1];
        bool deposit = r.kind == RequestKind.Deposit;
        if ((!deposit && r.kind != RequestKind.Redeem) || r.done || msg.sender != r.owner) {
            revert IVault.NotAllowed();
        }
        // Nor while a priced valuation waits to settle: its withdrawals are
        // paid from the cash first.
        if (block.timestamp < r.earliest + s.timers.fallbackAfter || s.valuing() || owed(s) || s.staging()) {
            revert IVault.NotYet();
        }
        // A closing vault pays its holders through its own door.
        if (!deposit && s.readyToClose()) revert IVault.WrongState();
        // Not while a valuation could price it: the fallback is for when
        // nothing can, not a way round the fee.
        if (s.lastPriced != 0 && s.latest().time >= r.earliest) revert IVault.NotAllowed();
        // A deposit goes back only when a valuation now would price nothing
        // either: otherwise its owner could record one, see the price, and
        // choose between buying and taking the money back.
        if (deposit && s.otherChains.length == 0 && _priceable(s)) revert IVault.NotAllowed();
        r.done = true;
        s.passDone();
        if (deposit) {
            s.escrow -= r.amount;
            _refund(s, requestId, r.owner, r.amount);
            return;
        }
        s.pendingShares -= r.amount;
        proRata(s, r.owner, r.amount);
    }

    /// Pay a claim's exit cost out of what it is paid, and say how much.
    function payExitCost(Layout storage s, uint256 claimId, uint256 payout) internal returns (uint256 cost) {
        cost = s.exitCost[claimId];
        if (cost == 0) return 0;
        if (cost > payout) cost = payout;
        s.exitCost[claimId] = 0;
        s.pay(s.exitCostTo[claimId], cost);
    }

    /// No fee: nothing can be priced. The leaver's units are a claim on
    /// their fraction of whatever the stations hold.
    function proRata(Layout storage s, address owner, uint256 shares) internal {
        // The holding fee up to now falls on every share, the leaver's too.
        syncHolding(s);
        // And the profit fee the shares owed at the last priced valuation,
        // as a slice of them, as a withdrawal pays it: the fallback is a way
        // out, never a way round the fee.
        if (s.lastPriced != 0) {
            Valuation storage last = s.latest();
            // The leaver's own part of the fee owed: with `p` the net price
            // per share, `m` the mark as the slot's deposits left it and `r`
            // the fee rate, the pool's gross price is (p - r m) / (1 - r) and
            // the part of a share owed as fee is r (p - m) / (p - r m). `p`
            // is the snapshot's net exit price; exits paid below the mark
            // since raise the real one a little, so the slice can come out a
            // little small, at the fee recipients' cost only.
            uint256 rate = uint256(s.fees.authorBps) + s.fees.platformBps;
            uint256 p = (last.grossExit - last.feeExit).mulDiv(PRICE, last.supply);
            uint256 m = s.mark;
            if (p > m) {
                uint256 slice = shares.mulDiv(rate * (p - m), BPS * p - rate * m, Math.Rounding.Ceil);
                if (slice > shares) slice = shares;
                _distribute(s, address(this), slice, 0);
                shares -= slice;
            }
        }
        uint256 assets = s.cash.mulDiv(shares, s.totalSupply + OFFSET);
        // The shares own what the units don't; these shares' part of that
        // is the leaver's for good, whatever fees are minted later.
        uint256 fraction = shares.mulDiv(WAD - s.liveUnits(), s.totalSupply + OFFSET);
        uint256 raw = fraction.mulDiv(WAD, s.scale());
        // The transaction fee stays in the pool, as for every other exit.
        uint256 kept = assets.mulDiv(s.fees.transactionBps, BPS, Math.Rounding.Ceil);
        s.burn(address(this), shares);
        s.cash -= assets - kept;
        assets -= kept;
        if (raw != 0) {
            s.accrue(owner);
            if (s.unitsOf[owner] == 0) s.unitsSince[owner] = uint64(block.timestamp);
            s.unitsOf[owner] += raw;
            s.units += raw;
            s.unitDebt[owner] = s.unitsOf[owner].mulDiv(s.perUnit, UNIT_SCALE, Math.Rounding.Ceil);
        }
        s.pay(owner, assets);
        emit IVault.ProRataTaken(owner, shares, assets, fraction);
    }

    function collectUnits() external returns (uint256 assets) {
        Layout storage s = VaultStorage.layout();
        s.accrue(msg.sender);
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
        uint256 raw = s.unitsOf[holder];
        if (raw == 0) revert IVault.Nothing();
        // Not between a valuation's pricing and its settlement, as claims;
        // not while a close is staged, nor before a valuation since one was.
        if (s.valuing() || owed(s) || s.staging() || s.valueFirst) revert IVault.NotYet();
        // On a vault that trades on other chains a serve here reaches only
        // this chain's station, and nothing on this chain can say what the
        // others hold: only the holder may accept it.
        if (s.otherChains.length != 0 && msg.sender != holder) revert IVault.NotAllowed();
        uint256 fraction = raw.mulDiv(s.scale(), WAD);
        if (fraction > WAD) fraction = WAD;
        if (fraction == 0) fraction = 1;
        (uint256 markBefore,) = IStation(s.homeStation).value();
        s.serving = holder;
        // A close that fails (one that would bring the holder nothing, say)
        // brings nothing home; whether that clears the units as dust or
        // leaves them waiting is judged below.
        uint256 home;
        bool blocked;
        bool staged;
        try IStation(s.homeStation).stageClose(fraction) returns (uint256 sent, bool outside, bool placed) {
            (home, blocked, staged) = (sent, outside, placed);
        } catch {}
        s.serving = address(0);
        // A close that settles after the block is staged for this holder:
        // what it fetches is theirs alone, credited when it is delivered.
        if (staged) {
            s.stagedHolder = holder;
            s.stagedFraction = fraction;
            return;
        }
        // Units are only burned for what came home for them: nothing home
        // from a station that held something, or from a vault whose stations
        // are elsewhere too, is not a serve. Units of an empty station are
        // worth nothing and can be cleared.
        // Units worth less than one unit of the dollar are worth nothing and
        // are cleared rather than left to block the station.
        // Judged at the mark, on every station: this one now, and all of them
        // as the last valuation counted them, whichever is more, as a claim's
        // dust is; a close refused because it would pay the holder nothing
        // at exit prices proves nothing, and the units wait. Once the vault
        // has closed its stations were found empty, and only what this one
        // holds now counts.
        uint256 stations = markBefore;
        if (s.lastPriced != 0 && s.state != VaultState.Closed) {
            Valuation storage last = s.latest();
            uint256 counted = last.gross - 1 - last.cash;
            if (counted > stations) stations = counted;
        }
        // The station rounds each venue's part and its idle part down on its
        // own, so units worth fewer dollar-units than it has parts may bring
        // nothing home however often they are served: they are dust.
        bool worthSomething = stations.mulDiv(fraction, WAD) > s.dust();
        if (blocked || (home == 0 && worthSomething)) {
            revert IVault.NotYet();
        }
        s.retireUnits(holder, fraction);
    }

    // The home station ------------------------------------------------------

    function toStation(uint256 amount) external {
        Layout storage s = VaultStorage.layout();
        if (msg.sender != s.homeStation) revert IVault.NotAllowed();
        // Nothing goes out while the vault is stopping, while unit holders
        // are owed part of what is out (sending more would need a price), or
        // while a claim's cash is home waiting to be priced: taking it back
        // out would leave the claim unpaid.
        // Nor between a valuation and its settlement: cash its withdrawals
        // were priced to be paid from stays home until they are.
        if (
            !s.live() || s.halted() || s.units != 0 || s.queuedClaims != 0 || s.valuing() || owed(s)
                || s.staging()
        ) {
            revert IVault.WrongState();
        }
        // The stations as last valued, the cash as it is now, and what was
        // sent out since: money that joined or left counts, and a run of
        // pulls can't shrink the value the floor is measured on.
        uint256 value = s.cash + s.sentTotal + 1;
        if (s.lastPriced != 0) {
            Valuation storage v = s.latest();
            value = value + (v.gross - 1 - v.cash) - s.sentAtValuation[s.lastPriced - 1];
        }
        uint256 floor = value.mulDiv(floorNow(s), BPS, Math.Rounding.Ceil);
        if (amount > s.free() || s.free() - amount < floor) revert IVault.BelowFloor();
        s.cash -= amount;
        s.sentTotal += amount;
        IERC20(s.asset).safeTransfer(msg.sender, amount);
        emit IVault.ToStation(amount);
    }

    function fromStation(uint256 amount) external {
        Layout storage s = VaultStorage.layout();
        if (msg.sender != s.homeStation) revert IVault.NotAllowed();
        IERC20(s.asset).safeTransferFrom(msg.sender, address(this), amount);
        if (s.closing) {
            s.cash += amount;
            emit IVault.FromStation(amount, 0);
            return;
        }
        address serving = s.serving;
        if (serving != address(0)) {
            s.unitCredit[serving] += amount;
            s.unitReserve += amount;
            emit IVault.FromStation(amount, amount);
            return;
        }
        uint256 toUnits;
        if (s.units != 0) {
            toUnits = amount.mulDiv(s.liveUnits(), WAD);
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
