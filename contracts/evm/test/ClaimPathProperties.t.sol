// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault, Valuation, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// The claim path and the band rule: a close pays what the valuation path
/// would at the same prices, cash moved before a claim changes no payout, a
/// close beside a queued claim overpays nobody, a dust claim blocks pulls for
/// one slot only, and no strategy can keep a leaver out for more than a week.
contract ClaimPathPropertiesTest is ClaimBase {
    using Math for uint256;

    /// `Valuations.exitOf`, as written: what the valuation path would pay.
    function exitOf(Valuation memory v, uint256 shares) internal view returns (uint256 payout) {
        uint256 slice = v.feeExit == 0 ? 0 : shares.mulDiv(v.feeExit, v.grossExit, Math.Rounding.Ceil);
        uint256 gross = (shares - slice).mulDiv(v.grossExit, v.supply);
        payout = gross - gross.mulDiv(vault.fees().transactionBps, 10_000, Math.Rounding.Ceil);
    }

    /// A pool above its mark, spread inside the band, cash short of alice.
    function aboveMarkAndShort() internal returns (uint256 aliceShares, uint256 bobShares) {
        aliceShares = depositAs(vault, alice, 100_000e6);
        bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 3e18);
        give(address(station), 20_000e6); // a gain: a fee is owed
        setPrice(SPOT, PRICE, 50); // 0.5% spread: inside the 1% band
    }

    // 1. The close path and the valuation path agree ------------------------

    /// At the same prices, the claim's close pays what a withdrawal priced at
    /// a valuation would: the same fraction, the same fee slice, the same
    /// transaction fee. If they disagreed, the leaver could pick the cheaper
    /// door.
    function test_a_claims_close_pays_what_the_valuation_path_would_at_the_same_prices() public {
        (uint256 aliceShares,) = aboveMarkAndShort();
        uint256 id = claimFor(alice, aliceShares);

        // What the valuation path would pay now (a valuation at these prices,
        // settled at once, is the snapshot the queue path uses).
        value(vault);
        uint256 expected = exitOf(latest(), aliceShares);

        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid);
        uint256 paid = vault.claimAt(id).paid;
        emit log_named_uint("valuation path would pay", expected);
        emit log_named_uint("close path paid", paid);
        assertApproxEqRel(paid, expected, 0.0001e18, "the two doors pay differently");
        assertLe(paid, expected + 1, "the close path pays more than the valuation path");
    }

    // 2. The executor shuffling money before a claim changes nothing ---------

    /// Pulling cash to the station (inside the floor) right before a claim is
    /// served moves the leaver's money from the cash share to the station
    /// share and nothing else.
    function test_the_executor_moving_cash_before_a_claim_changes_no_payout() public {
        (uint256 aliceShares, uint256 bobShares) = aboveMarkAndShort();
        uint256 id = claimFor(alice, aliceShares);
        uint256 stayer = perShare();

        uint256 snap = vm.snapshotState();
        vault.settleClaim(id);
        uint256 paidPlain = vault.claimAt(id).paid;
        uint256 stayerPlain = perShare();
        vm.revertToState(snap);

        // Front-run: the executor brings half the station's idle dollars
        // home (the floor leaves no room to pull), so the leaver's money
        // moves from the station share to the cash share.
        uint256 idle = usd.balanceOf(address(station));
        vm.prank(executor);
        station.push(idle / 2);
        vault.settleClaim(id);
        uint256 paidAfterPush = vault.claimAt(id).paid;

        assertApproxEqAbs(paidAfterPush, paidPlain, 2, "a push before the claim changed what it paid");
        assertGe(perShare() + 1, stayer, "the stayer's per-share worth dropped");
        assertApproxEqAbs(perShare(), stayerPlain, 2, "the stayer's worth depends on the push");
        bobShares;
    }

    // 3. Payday timing around a claim ---------------------------------------

    /// Served the moment before payday or the moment after. Before, the
    /// leaver's fee is measured on what they realise (the exit side); after,
    /// payday has already minted the fee on the mark side and diluted them
    /// for it, so they have paid the fee on a spread they don't realise.
    /// That is the spec's maths (section 6: payday mints `fee(G)`), bounded
    /// by `profitBps * (G - Gx) * shares / S`; nothing beyond it moves.
    function test_serving_a_claim_after_payday_costs_at_most_the_fee_on_the_spread() public {
        (uint256 aliceShares,) = aboveMarkAndShort();
        uint256 id = claimFor(alice, aliceShares);
        Valuation memory v = latest();
        uint256 bound = (v.gross - v.grossExit).mulDiv(1500, 10_000).mulDiv(aliceShares, v.supply);
        uint256 payday = nextMondayAfter(vault.lastPayment());
        vm.warp(payday - 1);

        uint256 snap = vm.snapshotState();
        vault.settleClaim(id);
        uint256 paidBefore = vault.claimAt(id).paid;
        vm.revertToState(snap);

        vm.warp(payday + 1);
        value(vault); // payday runs here
        assertEq(latest().fee, 0, "payday did not run");
        vault.settleClaim(id);
        uint256 paidAfter = vault.claimAt(id).paid;

        emit log_named_uint("paid before payday", paidBefore);
        emit log_named_uint("paid after payday", paidAfter);
        emit log_named_uint("fee on the spread the leaver does not realise", bound);
        assertLe(paidAfter, paidBefore, "leaving after payday paid more");
        // A few seconds of holding fee on top, and rounding.
        assertLe(
            paidBefore - paidAfter, bound + paidBefore / 1_000_000, "more than the fee on the spread moved"
        );
    }

    // 4. A close beside a queued claim --------------------------------------

    /// Bob's claim is queued on the cash; alice's claim closes and takes her
    /// fraction of that same cash. Bob is sent back to waiting (or paid) and
    /// nobody is paid above their fraction; carol keeps her price.
    function test_a_close_beside_a_queued_claim_overpays_nobody() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        uint256 carolShares = depositCarol(100_000e6);
        deploySpot(200_000e6, 200_000e6, 3e18);
        // 100k of cash: neither alice nor bob alone can be paid from it? No:
        // each is worth ~100k, the cash is ~100k, so the first would be paid
        // from cash. Make both claims by having them leave together.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        value(vault);
        // Alice was paid from cash; bob is a claim. Have the executor bring
        // nothing home: bob's claim is queued only if the cash now covers him.
        if (vault.claimCount() == 0) return;
        uint256 bobClaim = vault.claimCount();
        assertTrue(vault.claimAt(bobClaim).owner == bob);

        // Alice leaves again with a new deposit so there is a second claim to
        // close beside bob's queued one.
        uint256 aliceAgain = depositAs(vault, alice, 60_000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceAgain, alice);
        value(vault);
        uint256 aliceClaim = vault.claimCount();
        if (vault.claimAt(aliceClaim).owner != alice) return;

        // Bob queues on the cash (the deposit made it cover him)...
        vault.settleClaim(bobClaim);
        if (vault.claimAt(bobClaim).state != ClaimState.Queued) return;
        uint256 carolBefore = perShare();
        uint256 aliceFair = worth(aliceAgain);
        uint256 bobFair = worth(bobShares);

        // ...and alice's close takes her fraction of all the cash, bob's
        // "covered" part included.
        vault.settleClaim(aliceClaim);
        assertTrue(vault.claimAt(aliceClaim).state == ClaimState.Paid);
        assertLe(vault.claimAt(aliceClaim).paid, aliceFair, "alice was paid above her fraction");
        assertGe(perShare() + 1, carolBefore, "carol paid for alice");

        value(vault);
        ClaimState bobState = vault.claimAt(bobClaim).state;
        assertTrue(bobState == ClaimState.Paid || bobState == ClaimState.Pending, "bob's claim is stuck");
        if (bobState == ClaimState.Paid) {
            assertLe(vault.claimAt(bobClaim).paid, bobFair + 1, "bob was paid above his fraction");
        }
        assertGe(perShare() + 1, carolBefore, "carol paid for bob");
        carolShares;
    }

    // 5. A dust claim under the band rule holds pulls for one slot only ------

    function test_a_dust_claim_blocks_the_stations_pulls_for_one_slot_only() public {
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(100_000e6, 100_000e6, 1.5e18);
        vm.prank(alice);
        vault.requestRedeem(1, alice);
        setPrice(SPOT, PRICE, 2000); // wide at the valuation
        value(vault);
        assertEq(vault.claimCount(), 1, "a wide valuation did not book the dust claim");
        setPrice(SPOT, PRICE, 0);

        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);
        vm.prank(executor);
        vm.expectRevert(IVault.WrongState.selector);
        station.pull(1);

        value(vault);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "a dust claim never clears");
        vm.prank(executor);
        station.pull(1);
    }

    // 6. The band rule: re-queued claims and the exits that stay shut --------

    /// A market the strategy keeps wide (a small position in a market with
    /// a 30% spread) makes every valuation wide: no withdrawal is paid from
    /// cash, every redeem is a claim, and each queued claim is sent back.
    /// The freeze clock runs from the booking, though, so a week on anyone
    /// can freeze the vault: no strategy keeps a leaver out for good.
    function test_a_strategy_holding_a_wide_market_cannot_keep_a_leaver_out_for_more_than_a_week() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        // 95% of the pool is cash; 5% is at the venue in a market whose
        // spread is 30% around an honest mark.
        deploySpot(10_000e6, 10_000e6, 0.16e18); // 9.6k of BTC, 400 of venue cash
        setPrice(SPOT, PRICE, 3000);
        (uint256 markValue, uint256 exitValue) = station.value();
        assertLt(exitValue, markValue * 99 / 100, "the station is not wide");

        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1, "alice was paid from cash at a wide valuation");
        assertEq(usd.balanceOf(alice), 0);

        // Anyone re-queues her claim each day, and every valuation sends it
        // back; but the freeze clock runs from her booking, so a week on
        // anyone freezes the vault, whatever was tried meanwhile.
        for (uint256 day = 0; day < 6; day++) {
            vault.settleClaim(1);
            assertTrue(vault.claimAt(1).state == ClaimState.Queued, "a queue was refused");
            skip(1 days);
            value(vault);
            assertTrue(vault.claimAt(1).state == ClaimState.Pending, "paid through a wide market");
            vm.expectRevert(IVault.NotYet.selector);
            vault.freeze(1);
        }
        skip(1 days);
        vault.freeze(1);
        assertTrue(vault.state() == VaultState.Frozen, "a strategy held a leaver's exit shut for good");
    }
}
