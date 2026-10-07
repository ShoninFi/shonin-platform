// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {
    ClaimState,
    IVault,
    Request,
    StationPost,
    Valuation,
    ValuationStatus
} from "../../src/interfaces/IVault.sol";
import {ClaimBase} from "../ClaimBase.sol";
import {TwoChainVault} from "../TwoChainVault.sol";
import {Vm} from "forge-std/Vm.sol";

/// Forward pricing on a single-chain vault, under any timing and any order:
/// a request is settled at the first priced valuation at or after its
/// `earliest` and never at one before it; a valuation outside its slot's
/// window is refused; a void valuation prices nothing and its requests roll
/// on; one slot pays one price whatever order its requests were made in and
/// however `settle` is chunked; and the fallback opens only once nothing
/// could price a request for `fallbackAfter`, never over a priced valuation.
contract ForwardPricingTest is ClaimBase {
    uint256 internal constant OFFSET = 1e12;
    uint256 internal constant FALLBACK = 24 hours;

    /// The times of the priced valuations, oldest first.
    uint64[] internal priced;
    /// Requests whose settlement has been checked.
    mapping(uint256 => bool) internal checked;
    bool internal isWide;

    function setUp() public override {
        super.setUp();
        depositAs(vault, alice, 100_000e6);
        // Half a BTC at the venue, so a spread can put the market outside
        // the band, and the rest of 40k as the venue account's cash.
        deploySpot(40_000e6, 40_000e6, 0.5e18);
    }

    // Helpers ---------------------------------------------------------------

    /// The venue's spread wide enough that a close would be refused, or
    /// back to nothing.
    function wide(bool on) internal {
        isWide = on;
        setPrice(SPOT, PRICE, on ? 500 : 0);
    }

    function now_() internal view returns (uint256) {
        return vm.getBlockTimestamp();
    }

    function nextBoundary() internal view returns (uint256) {
        uint256 t = now_();
        return t - (t % SLOT) + SLOT;
    }

    /// Whether a priced valuation still has a request waiting on it.
    function owedNow() internal view returns (bool) {
        if (priced.length == 0) return false;
        uint64 last = priced[priced.length - 1];
        for (uint256 id = 1; id <= vault.requestCount(); id++) {
            Request memory r = vault.requestAt(id);
            if (!r.done && r.earliest <= last) return true;
        }
        return false;
    }

    function allDone() internal view returns (bool) {
        for (uint256 id = 1; id <= vault.requestCount(); id++) {
            if (!vault.requestAt(id).done) return false;
        }
        return true;
    }

    /// Try to record a valuation now, and check it is refused for exactly
    /// the reason the rules give, or recorded at this slot.
    function tryRecord() internal {
        uint256 t = now_();
        uint64 slot = uint64(t - (t % SLOT));
        uint256 count = vault.valuationCount();
        bytes4 expected;
        if (t - slot > 3) {
            expected = IVault.OutsideWindow.selector;
        } else if (count != 0 && vault.valuationAt(count - 1).time >= slot) {
            expected = IVault.AlreadyValued.selector;
        } else if (owedNow()) {
            expected = IVault.Unsettled.selector;
        }

        try vault.recordValuation() {
            assertEq(expected, bytes4(0), "a valuation the rules refuse was recorded");
            Valuation memory v = vault.valuationAt(count);
            assertEq(v.time, slot, "a valuation was recorded at another slot's time");
            if (isWide) {
                assertTrue(v.status == ValuationStatus.Void, "a market outside the band was priced");
            } else {
                assertTrue(v.status == ValuationStatus.Priced, "a sound market was not priced");
                priced.push(slot);
            }
        } catch (bytes memory reason) {
            assertEq(bytes4(reason), expected, "a valuation was refused for the wrong reason");
            assertEq(vault.valuationCount(), count, "a refused valuation was kept");
        }
    }

    /// Every request settled since the last look was settled at the newest
    /// priced valuation: one at or after its `earliest`, and the first such.
    function checkSettlements() internal {
        for (uint256 id = 1; id <= vault.requestCount(); id++) {
            Request memory r = vault.requestAt(id);
            if (!r.done || checked[id]) continue;
            checked[id] = true;
            assertGt(priced.length, 0, "a request settled before anything was priced");
            uint64 at = priced[priced.length - 1];
            assertLe(r.earliest, at, "a request settled at a valuation recorded before its earliest");
            if (priced.length > 1) {
                assertGt(
                    r.earliest,
                    priced[priced.length - 2],
                    "a request skipped the first valuation that could price it"
                );
            }
        }
    }

    // A request never settles before its earliest ---------------------------

    /// Deposits, withdrawals, valuations inside and outside their window,
    /// `settle` in chunks and a market moving in and out of the band, in any
    /// order and at any moment: each request settles at the first priced
    /// valuation at or after its `earliest`, a void one prices nothing, and
    /// every request is settled once the market is back.
    function testFuzz_requests_settle_at_the_first_priced_valuation_at_or_after_their_earliest(uint256 seed)
        public
    {
        // The valuation that priced alice's first deposit.
        priced.push(vault.valuationAt(vault.valuationCount() - 1).time);
        checked[1] = true;
        for (uint256 step = 0; step < 48; step++) {
            uint256 r = uint256(keccak256(abi.encode(seed, step)));
            uint256 action = r % 6;
            r >>= 8;
            if (action == 0) {
                if (r % 2 == 0) skip((r >> 8) % 23);
                else vm.warp(nextBoundary() + (r >> 8) % 6);
            } else if (action == 1) {
                requestDepositAs(vault, r % 2 == 0 ? alice : bob, MINIMUM_DEPOSIT + (r >> 8) % 5000e6);
            } else if (action == 2) {
                uint256 held = vault.balanceOf(alice);
                if (held > 1) {
                    vm.prank(alice);
                    vault.requestRedeem(held / (2 + r % 8), alice);
                }
            } else if (action == 3) {
                tryRecord();
            } else if (action == 4) {
                vault.settle(1 + r % 4);
            } else {
                wide(!isWide);
            }
            checkSettlements();
        }

        // The market comes back and the keeper catches up.
        wide(false);
        for (uint256 k = 0; k < 8 && !allDone(); k++) {
            vault.settle(type(uint256).max);
            checkSettlements();
            vm.warp(nextBoundary());
            tryRecord();
            vault.settle(type(uint256).max);
            checkSettlements();
        }
        assertTrue(allDone(), "a request was never settled");
    }

    // A valuation outside its window is refused -----------------------------

    function testFuzz_a_valuation_outside_its_slot_window_is_refused(uint64 offset, uint32 later) public {
        vm.warp(nextBoundary() + uint256(later) * SLOT + offset % SLOT);
        uint256 count = vault.valuationCount();
        if (offset % SLOT > 3) {
            vm.expectRevert(IVault.OutsideWindow.selector);
            vault.recordValuation();
            assertEq(vault.valuationCount(), count, "a valuation outside its window was kept");
        } else {
            vault.recordValuation();
            assertEq(vault.valuationCount(), count + 1);
        }
    }

    // A void valuation rolls its requests on --------------------------------

    /// Any number of valuations with the market outside the band price
    /// nothing: no share is minted or burned for a request, no dollar moves,
    /// the mark stays; the requests settle at the first sound one.
    function testFuzz_a_void_valuation_prices_nothing_and_its_requests_roll_on(
        uint256 deposit,
        uint256 fraction,
        uint8 voids
    ) public {
        deposit = bound(deposit, MINIMUM_DEPOSIT, 1_000_000e6);
        uint256 aliceShares = vault.balanceOf(alice);
        uint256 redeem = aliceShares / bound(fraction, 2, 50);
        requestDepositAs(vault, bob, deposit);
        vm.prank(alice);
        vault.requestRedeem(redeem, alice);
        Request memory r = vault.requestAt(vault.requestCount());

        wide(true);
        uint256 cash = vault.cash();
        uint256 escrow = vault.escrow();
        uint256 mark = vault.mark();
        uint256 aliceUsd = usd.balanceOf(alice);
        uint256 n = 1 + voids % 4;
        for (uint256 k = 0; k < n; k++) {
            vm.warp(nextBoundary());
            vault.recordValuation();
            assertEq(vault.settle(type(uint256).max), 0, "a void valuation settled a request");
            Valuation memory v = vault.valuationAt(vault.valuationCount() - 1);
            assertTrue(v.status == ValuationStatus.Void, "a market outside the band was priced");
            assertGe(v.time, r.earliest);
            assertEq(vault.balanceOf(bob), 0, "a deposit was priced at a void valuation");
            assertEq(usd.balanceOf(alice), aliceUsd, "a withdrawal was paid at a void valuation");
            assertEq(vault.cash(), cash, "cash moved at a void valuation");
            assertEq(vault.escrow(), escrow, "escrow moved at a void valuation");
            assertEq(vault.mark(), mark, "the mark moved at a void valuation");
            assertFalse(
                vault.requestAt(vault.requestCount()).done, "a request was settled at a void valuation"
            );
        }

        wide(false);
        vm.warp(nextBoundary());
        vault.recordValuation();
        Valuation memory sound = vault.valuationAt(vault.valuationCount() - 1);
        assertTrue(sound.status == ValuationStatus.Priced);
        vault.settle(type(uint256).max);
        assertTrue(vault.requestAt(vault.requestCount()).done, "the withdrawal did not roll on");
        assertTrue(vault.requestAt(vault.requestCount() - 1).done, "the deposit did not roll on");
        assertGt(vault.balanceOf(bob), 0, "the deposit bought nothing at the first sound valuation");
    }

    // One slot, one price ---------------------------------------------------

    struct Slot {
        address[] users;
        uint256[] keys;
        /// Per request: who, whether it is a deposit, and its amount.
        uint256[] who;
        bool[] isDeposit;
        uint256[] amount;
    }

    /// Four holders, each with a deposit priced already.
    function holders(uint256 seed) internal returns (Slot memory s) {
        s.users = new address[](4);
        s.keys = new uint256[](4);
        for (uint256 i = 0; i < 4; i++) {
            (s.users[i], s.keys[i]) = makeAddrAndKey(string(abi.encodePacked("holder", i)));
            uint256 amount = 1000e6 + uint256(keccak256(abi.encode(seed, "first", i))) % 50_000e6;
            give(s.users[i], amount);
            bytes memory statement =
                statementBy(s.keys[i], address(vault), s.users[i], vault.restrictedText());
            vm.startPrank(s.users[i]);
            usd.approve(address(vault), amount);
            vault.requestDeposit(amount, s.users[i], statement);
            vm.stopPrank();
        }
        value(vault);
    }

    /// What one slot asks for: three to seven deposits and withdrawals by
    /// the holders, each withdrawal small enough that all of one holder's
    /// fit in their shares together, and the cash covers them all.
    function plan(Slot memory s, uint256 seed) internal view {
        uint256 count = 3 + seed % 5;
        s.who = new uint256[](count);
        s.isDeposit = new bool[](count);
        s.amount = new uint256[](count);
        for (uint256 i = 0; i < count; i++) {
            uint256 r = uint256(keccak256(abi.encode(seed, "request", i)));
            s.who[i] = r % 4;
            s.isDeposit[i] = (r >> 8) % 2 == 0;
            s.amount[i] = s.isDeposit[i]
                ? MINIMUM_DEPOSIT + (r >> 16) % 20_000e6
                : vault.balanceOf(s.users[s.who[i]]) / (8 + (r >> 16) % 8);
        }
    }

    function permutation(uint256 n, uint256 seed) internal pure returns (uint256[] memory p) {
        p = new uint256[](n);
        for (uint256 i = 0; i < n; i++) {
            p[i] = i;
        }
        for (uint256 i = n; i > 1; i--) {
            uint256 j = uint256(keccak256(abi.encode(seed, i))) % i;
            (p[i - 1], p[j]) = (p[j], p[i - 1]);
        }
    }

    /// Ask for the slot's requests in `order` at one moment, value the slot
    /// at `when` and settle it in chunks drawn from `chunks`. Returns, per
    /// request of the plan, the shares a deposit bought or the dollars a
    /// withdrawal paid.
    function runSlot(
        Slot memory s,
        uint256[] memory order,
        uint256 chunks,
        uint256 when
    ) internal returns (uint256[] memory outcome) {
        uint256 first = vault.requestCount() + 1;
        for (uint256 k = 0; k < order.length; k++) {
            uint256 i = order[k];
            address who = s.users[s.who[i]];
            if (s.isDeposit[i]) {
                give(who, s.amount[i]);
                vm.startPrank(who);
                usd.approve(address(vault), s.amount[i]);
                vault.requestDeposit(s.amount[i], who, "");
                vm.stopPrank();
            } else {
                vm.prank(who);
                vault.requestRedeem(s.amount[i], who);
            }
        }
        vm.warp(when);
        vm.recordLogs();
        vault.recordValuation();
        for (uint256 k = 0; k < 64; k++) {
            if (allDone()) break;
            vault.settle(1 + uint256(keccak256(abi.encode(chunks, k))) % 3);
        }
        assertTrue(allDone(), "the slot never finished settling");

        outcome = outcomes(order, first);
    }

    /// Per request of the plan, from the slot's events: the shares a
    /// deposit bought or the dollars a withdrawal paid.
    function outcomes(
        uint256[] memory order,
        uint256 first
    ) internal view returns (uint256[] memory outcome) {
        outcome = new uint256[](order.length);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        bytes32 deposited = keccak256("Deposited(uint256,address,uint256,uint256)");
        bytes32 withdrawn = keccak256("Withdrawn(uint256,address,uint256,uint256,uint256)");
        for (uint256 l = 0; l < logs.length; l++) {
            if (logs[l].emitter != address(vault)) continue;
            bytes32 topic = logs[l].topics[0];
            if (topic != deposited && topic != withdrawn) continue;
            uint256 i = order[uint256(logs[l].topics[1]) - first];
            if (topic == deposited) {
                (, outcome[i]) = abi.decode(logs[l].data, (uint256, uint256));
            } else {
                (, outcome[i],) = abi.decode(logs[l].data, (uint256, uint256, uint256));
            }
        }
    }

    /// Every withdrawal of the slot paid exactly what its shares came to at
    /// the snapshot, and every deposit bought at the same price per net
    /// dollar, up to the floor each one rounds by.
    function checkOnePrice(Slot memory s, uint256[] memory outcome) internal view {
        Valuation memory v = vault.valuationAt(vault.valuationCount() - 1);
        assertTrue(v.status == ValuationStatus.Priced);
        for (uint256 i = 0; i < outcome.length; i++) {
            if (s.isDeposit[i]) {
                assertGt(outcome[i], 0, "a deposit bought nothing");
                for (uint256 j = 0; j < i; j++) {
                    if (s.isDeposit[j]) samePrice(outcome[i], s.amount[i], outcome[j], s.amount[j]);
                }
            } else {
                assertEq(
                    outcome[i], exitAt(v, s.amount[i]), "a withdrawal was not paid at the slot's snapshot"
                );
            }
        }
    }

    /// BTC up by up to 14% (a profit fee is owed) or not, with a spread
    /// inside the band, or a bid a touch above the mark.
    function moveMarket(uint256 market) internal {
        uint256 p = PRICE * (100 + market % 15) / 100;
        if ((market >> 8) % 2 == 0) {
            vm.prank(operator);
            venue.setPrices(SPOT, p, p, p * 1001 / 1000, p * 1002 / 1000);
        } else {
            setPrice(SPOT, p, (market >> 16) % 150);
        }
    }

    /// Two deposits bought at one price per net dollar: with shares
    /// `floor(net * k)` for one `k`, the cross products differ by less than
    /// either net.
    function samePrice(uint256 sharesA, uint256 assetsA, uint256 sharesB, uint256 assetsB) internal pure {
        uint256 netA = assetsA - ceilDiv(assetsA * 10, 10_000);
        uint256 netB = assetsB - ceilDiv(assetsB * 10, 10_000);
        uint256 a = sharesA * netB;
        uint256 b = sharesB * netA;
        assertLe(a > b ? a - b : b - a, netA + netB, "two deposits in one slot paid two prices");
    }

    /// What `shares` are paid at the snapshot `v`: the fee slice off, the
    /// rest at the exit price, less the transaction fee.
    function exitAt(Valuation memory v, uint256 shares) internal pure returns (uint256) {
        uint256 slice = v.feeExit == 0 ? 0 : ceilDiv(shares * v.feeExit, v.grossExit);
        uint256 gross = (shares - slice) * v.grossExit / v.supply;
        return gross - ceilDiv(gross * 10, 10_000);
    }

    /// The same slot asked for in two orders and settled in two ways of
    /// chunking pays every request exactly the same, at one price.
    function testFuzz_one_slot_one_price_whatever_the_order_and_the_chunks(
        uint256 seed,
        uint256 orderSeed,
        uint256 chunks,
        uint256 market,
        bool payday
    ) public {
        Slot memory s = holders(seed);
        moveMarket(market);
        plan(s, seed);
        uint256 when = payday ? nextMondayAfter(vault.lastPayment()) + SLOT : nextBoundary() + SLOT;
        when -= when % SLOT;

        uint256 snapshot = vm.snapshotState();
        uint256[] memory inOrder = runSlot(s, permutation(s.who.length, 0), chunks, when);
        checkOnePrice(s, inOrder);
        vm.revertToState(snapshot);
        uint256[] memory shuffled = runSlot(s, permutation(s.who.length, orderSeed), ~chunks, when);
        checkOnePrice(s, shuffled);
        for (uint256 i = 0; i < inOrder.length; i++) {
            assertEq(shuffled[i], inOrder[i], "the order of the requests changed what one of them got");
        }
    }

    // The fallback ----------------------------------------------------------

    /// Bob holds a small position and asks to redeem all of it. Returns the
    /// request.
    function bobAsksOut() internal returns (uint256 id, Request memory r) {
        depositAs(vault, bob, 1000e6);
        uint256 shares = vault.balanceOf(bob);
        vm.prank(bob);
        id = vault.requestRedeem(shares, bob);
        r = vault.requestAt(id);
    }

    function tryFallback(bool claim, address who, uint256 id) internal returns (bool ok, bytes4 err) {
        vm.prank(who);
        (bool success, bytes memory reason) = address(vault).call(
            claim ? abi.encodeCall(IVault.claimInstead, (id)) : abi.encodeCall(IVault.takeProRata, (id))
        );
        return (success, success ? bytes4(0) : bytes4(reason));
    }

    /// With nothing able to price a withdrawal (the market held outside the
    /// band, any number of void valuations meanwhile), neither the fallback
    /// nor a claim instead opens before `fallbackAfter` from its earliest,
    /// both open from then on, and only for its owner.
    function testFuzz_the_fallback_opens_only_after_fallback_after(
        uint32 wait,
        uint8 voids,
        bool claim,
        address stranger
    ) public {
        vm.assume(stranger != bob);
        (uint256 id, Request memory r) = bobAsksOut();
        uint256 waited = bound(wait, 0, 2 days);
        wide(true);
        // Void valuations at moments spread over the wait.
        uint256 n = voids % 4;
        for (uint256 k = 0; k < n; k++) {
            uint256 at = r.earliest + waited * (k + 1) / (n + 1);
            at -= at % SLOT;
            uint256 last = vault.valuationAt(vault.valuationCount() - 1).time;
            if (at <= last || at < now_()) continue;
            vm.warp(at);
            vault.recordValuation();
            assertTrue(vault.valuationAt(vault.valuationCount() - 1).status == ValuationStatus.Void);
        }
        vm.warp(r.earliest + waited);

        (bool ok, bytes4 err) = tryFallback(claim, stranger, id);
        assertFalse(ok, "someone else took a leaver's fallback");
        assertEq(err, IVault.NotAllowed.selector);

        uint256 bobUsd = usd.balanceOf(bob);
        (ok, err) = tryFallback(claim, bob, id);
        if (waited < FALLBACK) {
            assertFalse(ok, "the fallback opened before fallbackAfter");
            assertEq(err, IVault.NotYet.selector);
            return;
        }
        assertTrue(ok, "the fallback stayed shut after fallbackAfter with nothing priced");
        assertTrue(vault.requestAt(id).done);
        if (claim) {
            assertEq(
                vault.claimAt(vault.claimCount()).bookedAt, r.earliest, "the claim was not booked from due"
            );
        } else {
            assertGt(usd.balanceOf(bob), bobUsd, "the fallback paid nothing of the cash");
        }
    }

    /// A priced valuation that hasn't settled holds the fallback shut, at
    /// any wait: one that prices the withdrawal itself (and then settles it,
    /// so there is nothing left to fall back), and one that prices only an
    /// earlier request (after which the fallback opens once its delay has
    /// passed).
    function testFuzz_the_fallback_never_opens_over_a_priced_valuation(
        uint32 wait,
        bool pricesIt,
        bool claim
    ) public {
        depositAs(vault, bob, 1000e6);
        uint256 shares = vault.balanceOf(bob);
        uint256 waited = bound(wait, 0, 3 days);
        uint256 id;
        if (pricesIt) {
            vm.prank(bob);
            id = vault.requestRedeem(shares, bob);
            vm.warp(nextBoundary());
            vault.recordValuation();
        } else {
            // Alice's deposit is due at the next boundary, bob's withdrawal
            // a slot later; the valuation there prices only alice's.
            vm.warp(nextBoundary());
            requestDepositAs(vault, alice, 1000e6);
            skip(SLOT - 2);
            vm.prank(bob);
            id = vault.requestRedeem(shares, bob);
            vm.warp(nextBoundary());
            vault.recordValuation();
            assertLt(vault.valuationAt(vault.valuationCount() - 1).time, vault.requestAt(id).earliest);
        }
        vm.warp(vault.requestAt(id).earliest + waited);
        (bool ok, bytes4 err) = tryFallback(claim, bob, id);
        assertFalse(ok, "the fallback opened over a priced valuation waiting to settle");
        assertEq(err, IVault.NotYet.selector);

        vault.settle(type(uint256).max);
        (ok, err) = tryFallback(claim, bob, id);
        if (pricesIt) {
            assertFalse(ok, "a priced withdrawal fell back as well");
            assertEq(err, IVault.NotAllowed.selector);
        } else if (waited < FALLBACK) {
            assertFalse(ok);
            assertEq(err, IVault.NotYet.selector);
        } else {
            assertTrue(ok, "the fallback stayed shut once the valuation settled");
        }
    }

    /// A claim nobody could serve takes the fallback a day after it was
    /// booked, and not before, nor while a priced valuation waits to settle.
    function testFuzz_a_claim_takes_the_fallback_only_a_day_after_its_booking(
        uint32 wait,
        bool pending,
        address stranger
    ) public {
        vm.assume(stranger != bob);
        depositAs(vault, bob, 50_000e6);
        // The strategy holds nearly all the cash at the station: bob's
        // withdrawal can't be paid and is booked as a claim.
        uint256 keepHome = vault.cash() - 9000e6;
        vm.prank(executor);
        station.pull(keepHome);
        uint256 claimId = claimFor(bob, vault.balanceOf(bob));
        uint64 bookedAt = vault.claimAt(claimId).bookedAt;
        uint256 waited = bound(wait, SLOT, 3 days);
        vm.warp(bookedAt + waited);
        if (pending) {
            requestDepositAs(vault, alice, 1000e6);
            vm.warp(nextBoundary() + SLOT);
            vault.recordValuation();
        }

        vm.prank(stranger);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.takeProRataClaim(claimId);

        bool due = vm.getBlockTimestamp() >= bookedAt + FALLBACK;
        vm.prank(bob);
        if (pending || !due) {
            vm.expectRevert(IVault.NotYet.selector);
            vault.takeProRataClaim(claimId);
            if (!pending) return;
            vault.settle(type(uint256).max);
            if (!due) {
                vm.prank(bob);
                vm.expectRevert(IVault.NotYet.selector);
                vault.takeProRataClaim(claimId);
                return;
            }
            vm.prank(bob);
        }
        vault.takeProRataClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Fallback);
    }
}

/// Forward pricing on a vault that waits for another chain's posts: posts
/// outside their window never count, a valuation a station never posted to
/// or whose money in transit is overdue is void and rolls its requests on,
/// disagreeing posts price each side on its own figure, and the fallback is
/// shut while a valuation waits for posts.
contract ForwardPricingAcrossChainsTest is TwoChainVault {
    uint256 internal constant OFFSET = 1e12;
    uint256 internal constant FALLBACK = 24 hours;

    function ceilDiv(uint256 a, uint256 b) internal pure returns (uint256) {
        return a == 0 ? 0 : (a - 1) / b + 1;
    }

    function feeOn(uint256 mark, uint256 gross, uint256 supply) internal pure returns (uint256) {
        uint256 basis = ceilDiv(mark * supply, 1e30);
        return gross > basis ? (gross - basis) * 1500 / 10_000 : 0;
    }

    function nextMondayAfter(uint256 t) internal pure returns (uint256) {
        uint256 day = t / 1 days;
        return (day - ((day + 3) % 7)) * 1 days + 7 days;
    }

    /// Bless the vault's executor and unwinder again from now: a wait of
    /// days outlives a blessing.
    function refreshKeys() internal {
        blessExecutorKey(executor, address(vault), bundle);
        bytes memory journal = journalFor(UNWINDER_IMAGE, unwinder, leafFor(address(vault), bundle, unwinder));
        attestation.bless(
            sealOf(journal), journal, block.chainid, address(vault), bundle, unwinder, new bytes32[](0)
        );
    }

    function requestOut(address who, uint256 shares) internal returns (uint256 id) {
        vm.prank(who);
        id = vault.requestRedeem(shares, who);
    }

    // Disagreeing posts -----------------------------------------------------

    /// What the posts said, combined: the lowest mark, the lowest exit
    /// (never above its own post's mark) and the highest figure.
    struct Figures {
        uint256 lowMark;
        uint256 lowExit;
        uint256 top;
    }

    function postAll(
        uint64 time,
        uint64[10] memory raw,
        uint8 count,
        uint8 signers
    ) internal returns (Figures memory f) {
        f.lowMark = type(uint256).max;
        f.lowExit = type(uint256).max;
        uint256 n = 1 + count % 5;
        for (uint256 i = 0; i < n; i++) {
            uint256 m = uint256(raw[2 * i]) % 200_000e6;
            uint256 x = uint256(raw[2 * i + 1]) % 200_000e6;
            post(time, postOf(m, x), (signers >> i) % 2 == 0 ? EXECUTOR_KEY : UNWINDER_KEY);
            if (m < f.lowMark) f.lowMark = m;
            if ((x > m ? m : x) < f.lowExit) f.lowExit = x > m ? m : x;
            if ((x > m ? x : m) > f.top) f.top = x > m ? x : m;
        }
    }

    /// The fee shares a payday minted, from its event.
    function mintedOnPayday(Vm.Log[] memory logs) internal pure returns (uint256 minted) {
        for (uint256 l = 0; l < logs.length; l++) {
            if (logs[l].topics[0] == keccak256("FeesPaid(uint256,uint256,uint256,uint256)")) {
                (uint256 a, uint256 p,,) = abi.decode(logs[l].data, (uint256, uint256, uint256, uint256));
                minted = a + p;
            }
        }
    }

    /// Any number of posts from either blessed key, disagreeing in any way:
    /// fees and payday are measured on the lowest mark, a withdrawal is paid
    /// at the lowest exit (never above the lowest mark), and a deposit pays
    /// the highest figure any post carried.
    function testFuzz_disagreeing_posts_price_each_side_on_its_own_figure(
        uint64[10] memory raw,
        uint8 count,
        uint8 signers,
        bool payday
    ) public {
        depositTwo(alice, 100_000e6);
        uint256 aliceOut = vault.balanceOf(alice) / 10;
        requestOut(alice, aliceOut);
        requestDepositAs(vault, bob, 50_000e6);
        if (payday) {
            vm.warp(nextMondayAfter(vault.lastPayment()) - SLOT);
            refreshKeys();
        }
        uint64 time = open();
        Figures memory f = postAll(time, raw, count, signers);
        uint256 markBefore = vault.mark();
        vm.warp(time + DEADLINE + 1);
        vm.recordLogs();
        vault.closeValuation(time);
        Valuation memory v = latest();
        assertTrue(v.status == ValuationStatus.Priced);
        assertEq(v.gross, v.cash + f.lowMark + 1, "the mark side is not the lowest mark");
        assertEq(v.grossExit, v.cash + f.lowExit + 1, "the exit side is not the lowest exit");

        uint256 minted = mintedOnPayday(vm.getRecordedLogs());
        if (payday) {
            // Payday's fee shares are what the lowest mark owes, and no more.
            uint256 before = v.supply - minted;
            uint256 owed = feeOn(markBefore, v.gross, before);
            assertEq(
                minted, owed == 0 ? 0 : owed * before / (v.gross - owed), "payday not on the lowest mark"
            );
        } else {
            assertEq(minted, 0);
            assertEq(v.fee, feeOn(markBefore, v.gross, v.supply), "the fee owed is not on the lowest mark");
        }

        uint256 markPriced = vault.mark();
        uint256 aliceUsd = usd.balanceOf(alice);
        vault.settle(type(uint256).max);
        // Alice's withdrawal, at the lowest exit.
        uint256 slice = v.feeExit == 0 ? 0 : ceilDiv(aliceOut * v.feeExit, v.grossExit);
        uint256 gross = (aliceOut - slice) * v.grossExit / v.supply;
        assertEq(
            usd.balanceOf(alice) - aliceUsd,
            gross - ceilDiv(gross * 10, 10_000),
            "not paid at the lowest exit"
        );
        // Bob's deposit buys at most what the highest figure's price buys.
        uint256 gTop = v.cash + f.top + 1;
        uint256 feeTop = feeOn(markPriced, gTop, v.supply);
        assertLe(
            vault.balanceOf(bob),
            (50_000e6 - 50e6) * v.supply / (gTop - feeTop - 1),
            "bought below the highest post"
        );
    }

    // Void valuations roll their requests on --------------------------------

    /// Any run of void valuations, each for a reason of its own (the other
    /// station never posted, its post found a market outside the band, or
    /// money sent to it is overdue), prices nothing; the requests settle at
    /// the first valuation that is priced.
    function testFuzz_void_valuations_roll_their_requests_on(
        uint8 reasons,
        uint8 voids,
        uint64 sendSize
    ) public {
        depositTwo(alice, 100_000e6);
        uint256 aliceOut = vault.balanceOf(alice) / 10;
        uint256 outId = requestOut(alice, aliceOut);
        requestDepositAs(vault, bob, 10_000e6);
        uint256 aliceUsd = usd.balanceOf(alice);
        uint256 escrow = vault.escrow();
        uint256 sent;
        uint256 sends;

        uint256 n = 1 + voids % 3;
        for (uint256 k = 0; k < n; k++) {
            uint256 reason = (reasons >> (2 * k)) % 3;
            if (reason == 2) {
                // Money sent to the other chain an hour and more ago, still
                // on its way.
                uint256 amount = bound(sendSize, 2e6, 10_000e6);
                vm.startPrank(executor);
                station.pull(amount);
                station.bridge("local-bridge", OTHER, amount, "");
                vm.stopPrank();
                sent += amount;
                sends++;
                skip(1 hours + 1 minutes);
            }
            uint64 time = open();
            if (reason == 1) {
                StationPost memory p = postOf(sent, sent);
                p.outsideBand = true;
                post(time, p, EXECUTOR_KEY);
            } else if (reason == 2) {
                StationPost memory p = postOf(0, 0);
                p.inFlightFromHome = new uint64[](sends);
                for (uint256 i = 0; i < sends; i++) {
                    p.inFlightFromHome[i] = uint64(i);
                }
                post(time, p, EXECUTOR_KEY);
            }
            closeAfterDeadline(time);
            assertTrue(latest().status == ValuationStatus.Void, "a valuation that should be void was priced");
            assertFalse(vault.requestAt(outId).done, "a request was settled at a void valuation");
            assertEq(vault.balanceOf(bob), 0, "a deposit was priced at a void valuation");
            assertEq(usd.balanceOf(alice), aliceUsd, "a withdrawal was paid at a void valuation");
            assertEq(vault.escrow(), escrow);
        }

        // Every send has landed at the other station, which posts it.
        valueWith(sent, sent);
        assertTrue(latest().status == ValuationStatus.Priced);
        assertTrue(vault.requestAt(outId).done, "the withdrawal did not roll on to the priced valuation");
        assertGt(usd.balanceOf(alice), aliceUsd, "the withdrawal was not paid");
        assertGt(vault.balanceOf(bob), 0, "the deposit was not priced");
    }

    /// Money on its way counts at par up to the transit timeout and voids
    /// the valuation once past it, unless all of it is dust.
    function testFuzz_late_transit_voids_exactly_past_its_timeout(uint32 gap, uint64 amount) public {
        depositTwo(alice, 100_000e6);
        requestDepositAs(vault, bob, 10_000e6);
        uint256 lateness = bound(gap, 1 hours - 50, 1 hours + 50);
        uint256 size = bound(amount, 1, 10e6);
        uint256 t = vm.getBlockTimestamp() + lateness + 3 * SLOT;
        uint64 time = uint64(t - (t % SLOT));
        vm.warp(time - lateness);
        vm.startPrank(executor);
        station.pull(size);
        station.bridge("local-bridge", OTHER, size, "");
        vm.stopPrank();
        vm.warp(time);
        vm.prank(executor);
        vault.recordValuation();
        StationPost memory p = postOf(0, 0);
        p.inFlightFromHome = new uint64[](1);
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);

        bool late = lateness > 1 hours;
        bool dust = size <= MINIMUM_DEPOSIT / 10;
        Valuation memory v = latest();
        if (late && !dust) {
            assertTrue(v.status == ValuationStatus.Void, "money past its timeout was priced");
            assertEq(vault.balanceOf(bob), 0);
        } else {
            assertTrue(v.status == ValuationStatus.Priced, "money in time, or dust, voided the valuation");
            assertEq(v.gross, v.cash + size + 1, "money in transit did not count at par");
            assertGt(vault.balanceOf(bob), 0);
        }
    }

    // Posts outside their window never count --------------------------------

    /// A post sent after the post deadline, or as of a moment outside the
    /// window (before a slot after the open, or after the deadline), is
    /// refused, and the valuation it would have completed is void.
    function testFuzz_a_post_outside_its_window_is_refused_and_never_counts(
        uint16 delay,
        uint16 asOf
    ) public {
        depositTwo(alice, 100_000e6);
        requestDepositAs(vault, bob, 10_000e6);
        uint64 time = open();
        uint64 openedAt = vault.openedAt(vault.valuationCount() - 1);
        uint256 sentAfter = bound(delay, 0, 2 * DEADLINE);
        StationPost memory p = postOf(0, 0);
        p.asOf = uint64(openedAt + bound(asOf, 0, 2 * DEADLINE));
        vm.warp(time + sentAfter);

        bytes4 expected;
        if (sentAfter > DEADLINE) expected = IVault.OutsideWindow.selector;
        else if (p.asOf < openedAt + SLOT || p.asOf > time + DEADLINE) expected = IVault.NotAllowed.selector;
        bytes memory signature = signed(EXECUTOR_KEY, vault.postDigest(time, OTHER, p));
        if (expected != bytes4(0)) vm.expectRevert(expected);
        vault.postStation(time, OTHER, p, signature);

        closeAfterDeadline(time);
        if (expected != bytes4(0)) {
            assertTrue(latest().status == ValuationStatus.Void, "a refused post completed a valuation");
            assertEq(vault.balanceOf(bob), 0);
        } else {
            assertTrue(latest().status == ValuationStatus.Priced);
            assertGt(vault.balanceOf(bob), 0);
        }
    }

    /// Only one slot's moment is a valuation's: recording outside the
    /// window is refused, for the vault's own keys too.
    function testFuzz_a_valuation_outside_its_slot_window_is_refused(uint64 offset) public {
        depositTwo(alice, 100_000e6);
        uint256 t = vm.getBlockTimestamp() + 2 * SLOT;
        vm.warp(t - (t % SLOT) + offset % SLOT);
        uint256 count = vault.valuationCount();
        vm.prank(executor);
        if (offset % SLOT > 3) {
            vm.expectRevert(IVault.OutsideWindow.selector);
            vault.recordValuation();
            assertEq(vault.valuationCount(), count);
        } else {
            vault.recordValuation();
            assertEq(vault.valuationCount(), count + 1);
        }
    }

    // The fallback is shut while a valuation waits for posts ----------------

    /// However long a withdrawal has gone unpriced, neither the fallback nor
    /// a claim instead is open at any moment of a valuation's post window;
    /// once it closes (void, nobody posted), it is.
    function testFuzz_the_fallback_is_shut_while_a_valuation_waits_for_posts(
        uint32 wait,
        uint16 at,
        bool claim
    ) public {
        depositTwo(alice, 100_000e6);
        depositTwo(bob, 1000e6);
        uint256 id = requestOut(bob, vault.balanceOf(bob));
        Request memory r = vault.requestAt(id);
        vm.warp(r.earliest + bound(wait, FALLBACK, 3 days));
        refreshKeys();
        uint64 time = open();
        vm.warp(time + bound(at, 0, DEADLINE));
        vm.prank(bob);
        vm.expectRevert(IVault.NotYet.selector);
        if (claim) vault.claimInstead(id);
        else vault.takeProRata(id);

        vm.warp(time + DEADLINE + 1);
        vault.closeValuation(time);
        assertTrue(latest().status == ValuationStatus.Void);
        vm.prank(bob);
        if (claim) vault.claimInstead(id);
        else vault.takeProRata(id);
        assertTrue(vault.requestAt(id).done);
    }

    /// A claim's fallback is shut while a valuation waits for posts, too.
    function testFuzz_a_claims_fallback_is_shut_while_a_valuation_waits_for_posts(
        uint32 wait,
        uint16 at
    ) public {
        depositTwo(alice, 100_000e6);
        depositTwo(bob, 50_000e6);
        vm.prank(executor);
        station.pull(140_000e6);
        requestOut(bob, vault.balanceOf(bob));
        valueWith(140_000e6, 140_000e6);
        uint256 claimId = vault.claimCount();
        assertTrue(vault.claimAt(claimId).state == ClaimState.Pending, "bob's withdrawal was not a claim");
        vm.warp(vault.claimAt(claimId).bookedAt + bound(wait, FALLBACK, 3 days));
        refreshKeys();
        uint64 time = open();
        vm.warp(time + bound(at, 0, DEADLINE));
        vm.prank(bob);
        vm.expectRevert(IVault.NotYet.selector);
        vault.takeProRataClaim(claimId);

        vm.warp(time + DEADLINE + 1);
        vault.closeValuation(time);
        vm.prank(bob);
        vault.takeProRataClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Fallback);
    }
}
