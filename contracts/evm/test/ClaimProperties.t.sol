// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState, IVault, Tank, Valuation} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// The claim path, closed and paid at spot, against the properties the
/// design relies on, with random sizes, spreads, gains and ordering, and
/// acting between a valuation and its settlement: a leaver is paid at most
/// their fraction, and nobody who stays is worse off per share.
contract ClaimPropertiesTest is ClaimBase {
    /// Section 16.1: the vault's balance covers everything it books.
    function assertSolvent(string memory when) internal view {
        Tank memory t = vault.tank();
        (, uint256 reserve) = vault.units();
        uint256 booked = vault.cash() + t.balance + vault.escrow() + reserve + vault.heldFor(alice)
            + vault.heldFor(bob) + vault.heldFor(carol) + vault.heldFor(author) + vault.heldFor(platform);
        assertGe(usd.balanceOf(address(vault)), booked, when);
    }

    // 1. Random sizes, spreads and gains: the leaver gets at most their
    //    fraction and the stayers' per-share worth never drops -------------

    function testFuzz_a_closed_claim_pays_at_most_its_fraction_and_moves_no_stayer(
        uint256 a,
        uint256 b,
        uint256 c,
        uint256 deployBps,
        uint256 sizeBps,
        int256 moveBps,
        uint256 spreadBps,
        uint256 leaveBps
    ) public {
        a = bound(a, 1000e6, 1_000_000e6);
        b = bound(b, 1000e6, 1_000_000e6);
        c = bound(c, 1000e6, 1_000_000e6);
        deployBps = bound(deployBps, 5000, 9400);
        sizeBps = bound(sizeBps, 1000, 9000);
        moveBps = bound(moveBps, -3000, 3000);
        spreadBps = bound(spreadBps, 0, 99);
        leaveBps = bound(leaveBps, 100, 10_000);

        uint256 aliceShares = depositAs(vault, alice, a);
        depositAs(vault, bob, b);
        depositCarol(c);
        uint256 total = a + b + c;
        uint256 deployed = total * deployBps / 10_000;
        uint256 size = deployed * sizeBps / 10_000 * 1e18 / PRICE;
        deploySpot(deployed, deployed, size);
        uint256 price = uint256(int256(PRICE) + int256(PRICE) * moveBps / 10_000);
        setPrice(SPOT, price, spreadBps);

        uint256 leaving = aliceShares * leaveBps / 10_000;
        vm.prank(alice);
        vault.requestRedeem(leaving, alice);
        value(vault);
        if (vault.claimCount() == 0) return; // the cash paid it: an ordinary withdrawal
        uint256 id = vault.claimCount();

        uint256 fair = worth(leaving);
        uint256 stayerBefore = perShare();
        uint256 sizeBefore = spotSize();
        uint256 supplyBefore = vault.totalSupply();
        vault.settleClaim(id);
        if (vault.claimAt(id).state != ClaimState.Paid) return;

        uint256 paid = usd.balanceOf(alice);
        assertLe(paid, fair, "the leaver was paid more than their fraction at exit prices");
        // The close took the leaver's fraction of the position and no more
        // (the holding sync inside the try moves the supply by a hair).
        uint256 expectedClosed = sizeBefore * leaving / supplyBefore;
        assertApproxEqRel(
            sizeBefore - spotSize(), expectedClosed, 0.000001e18, "the close took the wrong fraction"
        );
        assertGe(perShare() + 1, stayerBefore, "a stayer's per-share worth dropped");
        assertSolvent("after the claim");
    }

    // 2. A claim tried between a priced valuation and its settlement --------

    /// A deposit and a redeem are priced at the snapshot; the leaver's close
    /// waits until they are settled. The depositor's shares must still be
    /// worth what they paid, the redeemer must get no more than their
    /// fraction, and the stayer's per-share worth must not drop.
    function test_a_claim_tried_between_a_valuation_and_its_settlement_waits_and_cheats_nobody() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        uint256 carolShares = depositCarol(100_000e6);
        deploySpot(280_000e6, 280_000e6, 4e18);
        setPrice(SPOT, PRICE, 20);

        uint256 id = claimFor(alice, aliceShares);

        // Bob asks to redeem a tenth, carol asks to deposit more; the
        // valuation prices both, but nobody settles yet.
        vm.prank(bob);
        vault.requestRedeem(bobShares / 10, bob);
        bytes memory statement = statementBy(carolKey, address(vault), carol, vault.restrictedText());
        give(carol, 50_000e6);
        vm.startPrank(carol);
        usd.approve(address(vault), 50_000e6);
        vault.requestDeposit(50_000e6, carol, statement);
        vm.stopPrank();
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();

        // Alice's claim waits for the snapshot to settle first.
        vm.expectRevert(IVault.NotYet.selector);
        vault.settleClaim(id);
        uint256 bobFair = worth(bobShares / 10);
        vault.settle(type(uint256).max);
        uint256 bobPaid = usd.balanceOf(bob);

        // Then it is served, at spot.
        uint256 carolBefore = perShare();
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid);
        assertGe(perShare() + 1, carolBefore, "the stayer paid for the claim");
        assertLe(bobPaid, bobFair + 1, "the redeemer was paid above their fraction");
        uint256 carolNew = vault.balanceOf(carol) - carolShares;
        assertApproxEqRel(worth(carolNew), 50_000e6 * 999 / 1000, 0.002e18, "the depositor's shares are off");
        assertSolvent("after everything settled");
    }

    // 3. A bid above the mark on the claim path ------------------------------

    /// The valuation path clamps the exit to the mark. The claim path sells
    /// at the bid. The leaver may get more than the mark value, but it must
    /// come from the venue, never from the stayers, and the close must take
    /// exactly the leaver's fraction of the position.
    function test_a_bid_above_the_mark_on_the_claim_path_costs_the_stayers_nothing() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 3e18);
        uint256 id = claimFor(alice, aliceShares);

        // The bid sits 0.5% above the mark.
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 1005 / 1000, PRICE * 1010 / 1000);
        (uint256 markValue,) = station.value();
        uint256 bobAtMarkBefore = (vault.cash() + markValue) * bobShares / vault.totalSupply();
        uint256 sizeBefore = spotSize();
        uint256 fractionClosed = sizeBefore * aliceShares / vault.totalSupply();
        uint256 markWorth = (vault.cash() + markValue) * aliceShares / vault.totalSupply();

        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid);

        // She was paid above her mark worth: the venue's bid paid for it.
        assertGt(usd.balanceOf(alice), markWorth, "the bid above the mark was not realised");
        (markValue,) = station.value();
        uint256 bobAtMarkAfter = (vault.cash() + markValue) * bobShares / vault.totalSupply();
        assertGe(bobAtMarkAfter + 1, bobAtMarkBefore, "the stayer paid for a bid above the mark");
        assertApproxEqRel(
            sizeBefore - spotSize(), fractionClosed, 0.000001e18, "more than her fraction was sold"
        );
    }

    // 4. Two claims, both orders, with a deposit landing between them --------

    function testFuzz_two_claims_in_either_order_each_take_their_fraction(
        uint256 a,
        uint256 b,
        bool bobFirst,
        uint256 spreadBps,
        bool depositBetween
    ) public {
        a = bound(a, 1000e6, 500_000e6);
        b = bound(b, 1000e6, 500_000e6);
        spreadBps = bound(spreadBps, 0, 99);
        uint256 aliceShares = depositAs(vault, alice, a);
        uint256 bobShares = depositAs(vault, bob, b);
        depositCarol(200_000e6);
        uint256 total = a + b + 200_000e6;
        deploySpot(total * 9 / 10, total * 9 / 10, total * 8 / 10 * 1e18 / PRICE);
        setPrice(SPOT, PRICE, spreadBps);

        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        value(vault);
        if (vault.claimCount() != 2) return;

        uint256 carolBefore = perShare();
        uint256 first = bobFirst ? 2 : 1;
        uint256 second = bobFirst ? 1 : 2;
        uint256 firstFair = worth(vault.claimAt(first).shares);
        vault.settleClaim(first);
        assertLe(vault.claimAt(first).paid, firstFair, "the first leaver got more than their fraction");
        if (depositBetween) {
            // A newcomer joins at the price between the two closes.
            depositCarol(30_000e6);
            carolBefore = perShare();
        }
        uint256 secondFair = worth(vault.claimAt(second).shares);
        vault.settleClaim(second);
        assertLe(vault.claimAt(second).paid, secondFair, "the second leaver got more than their fraction");
        assertGe(perShare() + 1, carolBefore, "the stayer paid for two exits");
        assertSolvent("after two claims");
    }

    // 5. Payday and the holding fee around a claim ---------------------------

    /// A claim served the day before payday, on a pool above its mark: the
    /// author and platform get its fee in cash, payday mints the stayers'
    /// fee, and the stayer's price after payday is what it would have been
    /// without the claim, plus at most the transaction fee she left behind.
    function test_a_claim_before_payday_leaves_the_stayers_payday_price_alone() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 2e18);
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        give(address(station), 30_000e6); // a gain above the mark
        skip(1 days);
        value(vault);
        uint256 id = claimFor(alice, aliceShares);

        // The stayer's net worth now: his fraction less his own fee.
        uint256 mark = vault.mark();
        uint256 bobGross = worth(bobShares);
        uint256 bobBasis = mark * bobShares / 1e30;
        uint256 bobNet = bobGross - (bobGross - bobBasis) * 1500 / 10_000;
        uint256 authorBefore = usd.balanceOf(author);
        uint256 platformBefore = usd.balanceOf(platform);

        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid);
        uint256 feePaid = usd.balanceOf(author) + usd.balanceOf(platform) - authorBefore - platformBefore;
        uint256 kept = (vault.claimAt(id).paid + feePaid) / 999; // the 0.1% she left in the pool
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        // After payday the fee is minted; bob's gross worth is his net
        // worth before, less a week of holding fee, plus his part of the
        // transaction fee she left (he is now nearly the whole pool).
        uint256 bobAfter = worth(bobShares);
        assertGe(bobAfter + 1, bobNet - bobNet * 3 / 10_000, "the stayer's payday price fell after a claim");
        assertLe(bobAfter, bobNet + kept + 1, "the stayer gained more than the leaver's transaction fee");
    }

    // 6. A claim paid from the station's idle dollars only -------------------

    function test_a_claim_takes_only_its_fraction_of_idle_station_dollars() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 0, 0); // all idle at the station
        uint256 id = claimFor(alice, aliceShares);
        uint256 fair = worth(aliceShares);
        uint256 bobBefore = perShare();
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid);
        assertLe(usd.balanceOf(alice), fair);
        assertApproxEqRel(usd.balanceOf(alice), fair * 999 / 1000, 0.0005e18);
        assertGe(perShare() + 1, bobBefore);
    }

    // 7. The leaver who also holds units -------------------------------------

    /// A holder with units from an earlier fallback and shares from a later
    /// deposit claims the shares: the close must take only the shares' part,
    /// and their units must keep their worth.
    function test_a_claim_beside_the_leavers_own_units_takes_only_the_shares_part() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(150_000e6, 150_000e6, 2e18);
        vm.prank(alice);
        uint256 req = vault.requestRedeem(aliceShares, alice);
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRata(req);
        (uint256 aliceUnits,) = vault.unitsOf(alice);
        assertGt(aliceUnits, 0);
        value(vault);
        // Alice comes back with a deposit, then the station gains: her new
        // shares are worth more than the cash, so her redeem is a claim.
        uint256 aliceNew = depositAs(vault, alice, 50_000e6);
        give(address(station), 300_000e6);
        value(vault);
        (, uint256 exitBefore) = station.value();
        uint256 unitsWorth = exitBefore * aliceUnits / WAD;
        uint256 id = claimFor(alice, aliceNew);
        uint256 fair = worth(aliceNew);
        uint256 bobBefore = perShare();
        uint256 aliceCash = usd.balanceOf(alice);
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid);
        assertLe(usd.balanceOf(alice) - aliceCash, fair, "the claim took part of the units' money");
        (uint256 unitsAfter,) = vault.unitsOf(alice);
        (, uint256 exitAfter) = station.value();
        assertApproxEqRel(exitAfter * unitsAfter / WAD, unitsWorth, 0.0001e18, "her units lost value");
        assertGe(perShare() + 1, bobBefore);
        assertSolvent("after a claim beside units");
    }

    // 8. A tank fill ahead of a claim in the same slot -----------------------

    /// The author's auto-fill and a leaver's redeem are priced at the same
    /// valuation, the fill first. The fill takes what cash there is; the
    /// redeem becomes a claim and is paid its fraction from the close.
    /// Nobody gets more than their fraction.
    function test_a_tank_fill_ahead_of_a_redeem_takes_only_the_authors_shares() public {
        uint256 authorShares = depositAs(vault, author, 50_000e6);
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(230_000e6, 230_000e6, 3e18);
        vm.prank(author);
        vault.setTankKeep(MINIMUM_TANK + 15_000e6);
        vault.keep(0, 0, ""); // queues a 15k fill from the author's shares
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        uint256 bobBefore = perShare();
        value(vault);
        Tank memory t = vault.tank();
        assertApproxEqAbs(t.balance, MINIMUM_TANK + 15_000e6, 1e6, "the fill was not paid");
        assertLt(vault.balanceOf(author), authorShares, "the fill did not burn the author's shares");
        assertEq(vault.claimCount(), 1, "the redeem behind the fill was not a claim");
        assertGe(perShare() + 1, bobBefore, "the fill cost a stayer");
        uint256 fair = worth(aliceShares);
        vault.settleClaim(1);
        assertLe(vault.claimAt(1).paid, fair);
        assertGe(perShare() + 1, bobBefore, "the claim cost a stayer");
        bobShares;
    }

    // 9. Anyone can force a leaver's close at the band's edge ----------------

    /// Anyone may serve a claim, including when the venue's bid sits at the
    /// bottom of the band (1% under the oracle): the leaver sells there, the
    /// loss is bounded by the band, and the stayers keep their price.
    function test_a_forced_close_at_the_bottom_of_the_band_costs_the_leaver_at_most_the_band() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 3e18);
        uint256 id = claimFor(alice, aliceShares);
        // Bid 1% under the oracle: still inside the band.
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 9900 / 10_000, PRICE);
        (uint256 markValue,) = station.value();
        uint256 atMark = (vault.cash() + markValue) * aliceShares / vault.totalSupply();
        uint256 bobBefore = perShare();
        vm.prank(bob);
        vault.settleClaim(id);
        uint256 paid = usd.balanceOf(alice);
        emit log_named_uint("her worth at mark", atMark);
        emit log_named_uint("paid at the bottom of the band", paid);
        assertGe(paid, atMark * 9890 / 10_000, "a forced close cost more than the band and the fee");
        assertGe(perShare() + 1, bobBefore);
    }
}
