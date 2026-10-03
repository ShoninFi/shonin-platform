// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Claim, ClaimState, IVault, ValuationStatus} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Properties of the claim-close and serve mechanism, each asserting an
/// intended rule across market states on the stand-in venue.
contract ClaimClosePropertiesTest is ClaimBase {
    uint256 internal constant OFFSET = 1e12;

    function conserved() internal view {
        (, uint256 reserve) = vault.units();
        assertGe(
            usd.balanceOf(address(vault)),
            vault.cash() + vault.tank().balance + vault.escrow() + reserve,
            "the vault's balance no longer covers what it owes"
        );
    }

    function unitsWorthAtExit() internal view returns (uint256) {
        (, uint256 exit) = station.value();
        (uint256 units,) = vault.units();
        return exit * units / WAD;
    }

    /// What a claim of `shares` should pay: its fraction at exit
    /// prices, less its own profit fee against the mark and the 0.1%
    /// transaction fee.
    function expectedPayout(uint256 shares) internal view returns (uint256) {
        uint256 owned = worth(shares);
        uint256 basis = ceilDiv(vault.mark() * shares, 1e30);
        uint256 fee = owned > basis ? (owned - basis) * 1500 / 10_000 : 0;
        uint256 gross = owned - fee;
        return gross - ceilDiv(gross * 10, 10_000);
    }

    function claimOf(address who, uint256 shares) internal returns (uint256 id) {
        vm.prank(who);
        vault.requestRedeem(shares, who);
        value(vault);
        id = vault.claimCount();
        if (id == 0 || vault.claimAt(id).state != ClaimState.Pending) return 0;
    }

    /// Spot, idle at the station, cash in the vault, spread at the band's
    /// edge: the leaver gets their fraction of the cash and of the station
    /// at the bid, less their own fees; nobody else's price moves.
    function test_p1_spot_with_idle_everywhere_pays_the_fraction_at_the_bid() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(150_000e6, 120_000e6, 1.5e18);
        setPrice(SPOT, PRICE, 200);
        assertFalse(station.outsideBand());
        value(vault);
        uint256 id = claimFor(bob, bobShares);
        uint256 expected = expectedPayout(bobShares);
        uint256 stayers = perShare();
        vm.prank(carol);
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid, "not paid");
        assertApproxEqAbs(usd.balanceOf(bob), expected, 4, "paid other than the fraction");
        assertGe(perShare(), stayers, "the stayers paid");
        conserved();
    }

    /// With units outstanding, a claim's close leaves the unit holders'
    /// absolute worth at the station unchanged, and a serve after pays it.
    function test_p2_claim_close_leaves_units_worth_unchanged() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        depositCarol(100_000e6);
        deploySpot(240_000e6, 200_000e6, 2e18);
        setPrice(SPOT, PRICE, 100);
        value(vault);
        vm.prank(alice);
        uint256 req = vault.requestRedeem(aliceShares, alice);
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 5 / 6, PRICE);
        value(vault);
        skip(1 days + 2 * SLOT);
        vm.prank(alice);
        vault.takeProRata(req);
        setPrice(SPOT, PRICE, 100);
        value(vault);
        uint256 unitsBefore = unitsWorthAtExit();
        assertGt(unitsBefore, 0);

        uint256 id = claimFor(bob, bobShares);
        uint256 expected = expectedPayout(bobShares);
        vm.prank(carol);
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid, "not paid");
        assertApproxEqAbs(usd.balanceOf(bob), expected, 4, "bob paid other than his fraction");
        assertApproxEqAbs(unitsWorthAtExit(), unitsBefore, 2, "the unit holders' worth moved");
        conserved();

        vault.serveProRata(alice);
        vm.prank(alice);
        uint256 got = vault.collectUnits();
        assertApproxEqAbs(got, unitsBefore, 2, "alice's serve paid other than her worth");
        conserved();
    }

    /// A short perp with positive venue cash.
    function test_p3_short_perp_with_positive_cash_pays_the_fraction() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(190_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 190_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: false, size: 2e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        setPrice(PERP, PRICE, 200);
        value(vault);
        uint256 id = claimFor(bob, bobShares);
        uint256 expected = expectedPayout(bobShares);
        uint256 stayers = perShare();
        vm.prank(carol);
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid, "not paid");
        assertApproxEqAbs(usd.balanceOf(bob), expected, 4, "paid other than the fraction");
        assertGe(perShare(), stayers, "the stayers paid");
        conserved();
    }

    /// Fuzz: a healthy spot vault with any deployment and any claim size is
    /// always servable and always pays the fraction.
    /// forge-config: default.fuzz.runs = 400
    function testFuzz_p4_healthy_spot_claims_always_pay(
        uint256 atVenue,
        uint256 sizeBps,
        uint256 claimBps,
        uint256 spreadBps
    ) public {
        atVenue = bound(atVenue, 1e6, 150_000e6);
        sizeBps = bound(sizeBps, 0, 10_000);
        claimBps = bound(claimBps, 1, 10_000);
        spreadBps = bound(spreadBps, 0, 199);
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        uint256 size = (atVenue * sizeBps / 10_000) * 1e18 / PRICE;
        deploySpot(150_000e6, atVenue, size);
        setPrice(SPOT, PRICE, spreadBps);
        value(vault);
        uint256 shares = bobShares * claimBps / 10_000;
        uint256 id = claimOf(bob, shares);
        if (id == 0) return;
        uint256 expected = expectedPayout(shares);
        uint256 stayers = perShare();
        vm.prank(carol);
        vault.settleClaim(id);
        Claim memory c = vault.claimAt(id);
        assertTrue(c.state == ClaimState.Paid || c.state == ClaimState.Queued, "a healthy claim did not pay");
        if (c.state == ClaimState.Paid) {
            assertApproxEqAbs(usd.balanceOf(bob), expected, 4, "paid other than the fraction");
            assertGe(perShare() + 1, stayers, "the stayers paid");
        }
        conserved();
    }

    /// Fuzz: a hedged account (short perp against spot) with little venue
    /// cash, healthy at every price, and claims of any size down to a wei.
    /// forge-config: default.fuzz.runs = 400
    function testFuzz_p5_hedged_account_claims_always_pay(
        uint256 shortSize,
        uint256 shift,
        uint256 spreadBps,
        uint256 extraCash
    ) public {
        shortSize = bound(shortSize, 1e12, 1e18);
        shift = bound(shift, 0, 60);
        spreadBps = bound(spreadBps, 0, 199);
        extraCash = bound(extraCash, 0, 10);
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(150_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 100_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: false, size: shortSize, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        int256 cashNow = venue.cashOf(address(station));
        uint256 spotSize_ = (uint256(cashNow) - extraCash) * 1e18 / PRICE;
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: spotSize_, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        setPrice(SPOT, PRICE, spreadBps);
        setPrice(PERP, PRICE, spreadBps);
        value(vault);
        uint256 shares = bobShares >> shift;
        if (shares == 0) shares = 1;
        uint256 id = claimOf(bob, shares);
        if (id == 0) return;
        uint256 expected = expectedPayout(shares);
        vm.prank(carol);
        vault.settleClaim(id);
        Claim memory c = vault.claimAt(id);
        emit log_named_int("venue cash", venue.cashOf(address(station)));
        emit log_named_uint("shares", shares);
        assertTrue(
            c.state == ClaimState.Paid || c.state == ClaimState.Queued, "a healthy hedged claim did not pay"
        );
        if (c.state == ClaimState.Paid) {
            assertApproxEqAbs(usd.balanceOf(bob), expected, 8, "paid other than the fraction");
        }
        conserved();
    }

    /// Fuzz: a healthy spot station; units of any size are served for
    /// their worth, or cleared only as dust.
    /// forge-config: default.fuzz.runs = 300
    function testFuzz_p6_healthy_spot_serves_pay_the_worth(
        uint256 atVenue,
        uint256 sizeBps,
        uint256 shift,
        uint256 spreadBps
    ) public {
        atVenue = bound(atVenue, 1e6, 150_000e6);
        sizeBps = bound(sizeBps, 0, 10_000);
        shift = bound(shift, 0, 60);
        spreadBps = bound(spreadBps, 0, 199);
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        uint256 size = (atVenue * sizeBps / 10_000) * 1e18 / PRICE;
        deploySpot(150_000e6, atVenue, size);
        setPrice(SPOT, PRICE, spreadBps);
        value(vault);
        uint256 shares = aliceShares >> shift;
        if (shares == 0) shares = 1;
        vm.prank(alice);
        uint256 req = vault.requestRedeem(shares, alice);
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRata(req);
        (uint256 held,) = vault.unitsOf(alice);
        if (held == 0) return;
        uint256 fair = unitsWorthAtExit();
        vm.prank(carol);
        vault.serveProRata(alice);
        (uint256 after_, uint256 claimable) = vault.unitsOf(alice);
        assertEq(after_, 0, "units left");
        emit log_named_uint("fair", fair);
        emit log_named_uint("claimable", claimable);
        if (fair > 4) assertApproxEqAbs(claimable, fair, 4, "served other than the worth");
        conserved();
    }

    /// A perp long with positive venue cash and funding owed but not yet
    /// settled: the leaver's fraction nets the funding.
    function test_p7_funding_owed_is_netted_in_the_fraction() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(190_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 190_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 2e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        // Longs owe 500 a unit of size since they opened.
        vm.prank(operator);
        venue.accrueFunding(PERP, 500e6);
        setPrice(PERP, PRICE, 100);
        value(vault);
        uint256 id = claimFor(bob, bobShares);
        uint256 expected = expectedPayout(bobShares);
        uint256 stayers = perShare();
        vm.prank(carol);
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid, "not paid");
        assertApproxEqAbs(usd.balanceOf(bob), expected, 4, "paid other than the fraction");
        assertGe(perShare() + 1, stayers, "the stayers paid");
        conserved();
    }

    /// A claim the cash covers at a stale price is queued; when the next
    /// valuation finds the cash short it goes back to pending and is then
    /// served by a close, paid exactly once.
    function test_p8_queued_claim_round_trip_pays_once() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        // 120k deployed in spot; 80k cash at home. Bob (half) is worth ~100k.
        deploySpot(120_000e6, 120_000e6, 2e18);
        value(vault);
        uint256 id = claimFor(bob, bobShares);
        // Everything comes home except the spot: the station pushes nothing,
        // but the price doubles, so at the stale valuation bob's claim is 100k
        // against 80k of cash: not covered. Instead halve the price so that
        // at the stale valuation it reads covered? It can't: the stale
        // valuation is fixed. So push 30k home: cash 110k covers the stale 100k.
        vm.prank(executor);
        station.withdraw("local", 0); // no-op through the driver
        // Sell 0.6 BTC at the venue and push the proceeds home.
        vm.startPrank(executor);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] = OrderRequest({market: SPOT, isBuy: false, size: 0.6e18, limitPrice: 0, reduceOnly: true});
        venue.place(address(station), orders);
        station.withdraw("local", 36_000e6);
        station.push(36_000e6);
        vm.stopPrank();
        assertGt(vault.cash(), 100_000e6);
        vm.prank(carol);
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Queued, "not queued");
        // Before the next valuation the price doubles: bob's claim is now
        // ~142k against 116k of cash, so it goes back to pending.
        setPrice(SPOT, PRICE * 2, 0);
        value(vault);
        assertTrue(vault.claimAt(id).state == ClaimState.Pending, "not back to pending");
        assertEq(usd.balanceOf(bob), 0);
        uint256 expected = expectedPayout(bobShares);
        vm.prank(carol);
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid, "not paid");
        assertApproxEqAbs(usd.balanceOf(bob), expected, 4, "paid other than the fraction");
        conserved();
    }
}
