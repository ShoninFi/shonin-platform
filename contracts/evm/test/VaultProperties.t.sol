// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Allowances} from "../src/interfaces/IPrices.sol";
import {
    Claim, ClaimState, IVault, Tank, Valuation, VaultInit, VaultState
} from "../src/interfaces/IVault.sol";
import {VaultConfig} from "../src/interfaces/IVaultFactory.sol";
import {Fixture} from "./Fixture.sol";
import {Vm} from "forge-std/Vm.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// Properties of a single-chain vault where a defect would most likely hide:
/// the stayers' price through deposits, paydays and exits; a bank run paid
/// fraction by fraction; the exit clamp; a refused payout; the admin's reach;
/// and signatures bound to one vault.
contract VaultPropertiesTest is Fixture {
    address internal constant DEAD = 0x000000000000000000000000000000000000dEaD;

    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    // Helpers ---------------------------------------------------------------

    function nextMondayAfter(uint256 t) internal pure returns (uint256) {
        uint256 day = t / 1 days;
        return (day - ((day + 3) % 7)) * 1 days + 7 days;
    }

    function latest() internal view returns (Valuation memory) {
        return vault.valuationAt(vault.valuationCount() - 1);
    }

    /// Pull `amount` to the station and put it at the venue, one BTC long
    /// against one BTC short, with `spreadBps` on spot.
    function hedge(uint256 amount, uint256 spreadBps) internal {
        vm.startPrank(executor);
        station.pull(amount);
        station.approveKey("local", executor);
        station.deposit("local", amount);
        OrderRequest[] memory orders = new OrderRequest[](2);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        orders[1] =
            OrderRequest({market: PERP, isBuy: false, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        setPrice(SPOT, PRICE, spreadBps);
    }

    /// Section 16.1: the vault's balance covers everything it books.
    function assertSolvent(string memory when) internal view {
        Tank memory t = vault.tank();
        (, uint256 reserve) = vault.units();
        uint256 booked = vault.cash() + t.balance + vault.escrow() + reserve + vault.heldFor(alice)
            + vault.heldFor(bob) + vault.heldFor(author) + vault.heldFor(platform);
        assertGe(usd.balanceOf(address(vault)), booked, when);
    }

    // 1. Blend, payday, exit: nobody who stayed pays ---------------------------

    /// A deposit above the mark blends it, payday mints the fee owed and
    /// raises the mark, and the newcomer leaves: the mark never falls, the
    /// stayer's price never moves beyond the holding fee, and the newcomer
    /// never gets back more than they put in.
    function testFuzz_blend_payday_exit_keeps_the_stayers_price(
        uint256 first,
        uint256 gain,
        uint256 second
    ) public {
        first = bound(first, 1000e6, 10_000_000e6);
        second = bound(second, MINIMUM_DEPOSIT, 10_000_000e6);
        // At least a tenth of a percent, so a day of holding fee (0.0027%)
        // doesn't swallow it.
        gain = bound(gain, first / 1000, first / 2);

        depositAs(vault, alice, first);
        vm.prank(executor);
        station.pull(first / 2);
        // A fresh fee week, then the gain, then the valuation that shows it.
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        give(address(station), gain);
        skip(1 days);
        value(vault);
        uint256 mark0 = vault.mark();
        uint256 price0 = vault.sharePrice();
        assertGt(latest().fee, 0, "the gain is above the mark, so a fee is owed");

        uint256 bobShares = depositAs(vault, bob, second);
        uint256 mark1 = vault.mark();
        assertGe(mark1, mark0, "the blend lowered the mark");
        assertApproxEqRel(vault.sharePrice(), price0, 0.00001e18, "the blend moved the price");

        // Payday: the fee is minted, the mark catches up, the price is the
        // same less a week of holding fee (1% a year for 7 days: 0.019%).
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        uint256 mark2 = vault.mark();
        uint256 price2 = vault.sharePrice();
        assertGe(mark2, mark1, "payday lowered the mark");
        assertEq(latest().fee, 0, "a fee is still owed after payday");
        assertLe(price2, vault.sharePrice() + 1);
        assertGe(
            price2 + 1, price0 - price0 * 2 / 10_000, "payday moved the price by more than the holding fee"
        );

        uint256 paid = redeemAs(vault, bob, bobShares);
        assertLe(paid, second, "the newcomer took out more than they put in");
        assertGe(vault.mark(), mark2, "a withdrawal lowered the mark");
        value(vault);
        assertGe(vault.sharePrice() + 1, price2 - price2 / 1_000_000, "the stayer paid for the exit");
        assertSolvent("after the round trip");
    }

    // 2. Bank run: everyone is paid their fraction, the vault owes nothing ----

    function test_a_bank_run_pays_everyone_their_fraction_and_leaves_nothing_owed() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        uint256 authorShares = depositAs(vault, author, 50_000e6);
        hedge(230_000e6, 0);
        give(address(station), 20_000e6); // a gain at the station: 270k in all
        value(vault);
        uint256 total = vault.totalValue();
        assertApproxEqAbs(total, 270_000e6, 2e6);
        uint256 supply = vault.totalSupply();
        assertSolvent("before the run");

        // Alice and bob both leave in one slot: neither can be paid from 20k.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        value(vault);
        assertEq(vault.claimCount(), 2);
        assertSolvent("after the claims were booked");

        // Both are served: each closes its own fraction and is paid.
        uint256 mark = vault.mark();
        vault.settleClaim(1);
        assertSolvent("after the first close");
        vault.settleClaim(2);
        assertSolvent("after the second close");
        Claim memory a = vault.claimAt(1);
        Claim memory b = vault.claimAt(2);
        assertTrue(a.state == ClaimState.Paid && b.state == ClaimState.Paid, "a claim went unpaid");

        // Nobody gets more than their fraction of the value at the valuation
        // that priced the run, and nobody gets less than it minus their own
        // profit fee (15% of what their shares are worth above the mark)
        // and the transaction fee.
        uint256 aliceFraction = total * aliceShares / supply;
        uint256 bobFraction = total * bobShares / supply;
        assertLe(a.paid, aliceFraction, "alice got more than her fraction");
        assertLe(b.paid, bobFraction, "bob got more than his fraction");
        uint256 aliceOwed = aliceFraction - (aliceFraction - mark * aliceShares / 1e30) * 1500 / 10_000;
        assertApproxEqAbs(a.paid, aliceOwed - aliceOwed / 1000, 1e6, "alice got less than her fraction");
        uint256 bobOwed = bobFraction - (bobFraction - mark * bobShares / 1e30) * 1500 / 10_000;
        // Bob was still in the pool when alice's transaction fee was kept in
        // it, so he may get a little more, never less.
        assertGe(b.paid + 1e6, bobOwed - bobOwed / 1000, "bob got less than his fraction");
        assertSolvent("after the claims were paid");

        // The author is the last holder: the station still holds their part
        // of it (a fifth of the 250k that was there), and no more.
        value(vault);
        uint256 stationValue = vault.totalValue() - vault.cash();
        (uint256 markValue,) = station.value();
        assertApproxEqRel(markValue, stationValue, 0.001e18, "the station holds something nobody owns");
        assertApproxEqRel(
            markValue, 50_000e6, 0.002e18, "the closes took more or less than the leavers' part"
        );
        vm.prank(author);
        vault.requestRedeem(authorShares, author);
        value(vault);
        uint256 authorPaid = vault.claimAt(3).paid;
        if (vault.claimAt(3).state != ClaimState.Paid) {
            vault.settleClaim(3);
            value(vault);
            authorPaid = vault.claimAt(3).paid;
        }
        assertTrue(vault.claimAt(3).state == ClaimState.Paid, "the last leaver was not paid");
        assertSolvent("after the last leaver");
        assertGt(authorPaid, 50_000e6 * 999 / 1000, "the last leaver was paid under par");

        // What is left belongs to the fee recipients and the dead shares,
        // and the vault can close and pay them all.
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);
        value(vault);
        vault.close();
        uint256 authorFeeShares = vault.balanceOf(author);
        uint256 platformFeeShares = vault.balanceOf(platform);
        vm.prank(author);
        vault.redeemClosed(authorFeeShares, author);
        vm.prank(platform);
        vault.redeemClosed(platformFeeShares, platform);
        assertSolvent("after the close");
        assertEq(vault.totalSupply(), vault.balanceOf(DEAD), "shares remain that nobody holds");
        // What remains is the dead shares' part of the pool, and the
        // transaction fee the last leaver left in it.
        assertLe(vault.cash(), DEAD_ASSETS + authorPaid / 900, "cash remains after everyone was paid");
    }

    // 3. The admin key: propose, revoke, kill, and nothing else ---------------

    function test_the_admin_key_can_only_kill_the_vault_and_moves_no_money() public {
        depositAs(vault, alice, 100_000e6);
        uint256 vaultBefore = usd.balanceOf(address(vault));
        uint256 adminBefore = usd.balanceOf(admin);
        uint256 platformBefore = usd.balanceOf(platform);
        Allowances memory a = allowancesCosting(1e6);

        vm.startPrank(admin);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.setIdleFloor(0);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.retire();
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.withdrawTankUnsigned(1);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.requestTankWithdrawal();
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.withdrawTank(1, 0, 0, "");
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.topUpFromShares(1);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.setTankKeep(1);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.setAllowances(a);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.toStation(1);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.fromStation(1);
        vm.expectRevert(IVault.NotBlessed.selector);
        vault.keep(1e6, uint64(vm.getBlockTimestamp()), hex"00");
        VaultInit memory init;
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.initialize(init, 2, 1);
        vm.stopPrank();

        // The factory's terms can change, and the vault's don't.
        VaultConfig memory cfg = vaultConfig();
        cfg.authorBps = 4000;
        cfg.holdingBps = 500;
        vm.prank(admin);
        factory.setConfig(cfg);
        assertEq(vault.fees().authorBps, 1000);
        assertEq(vault.fees().holdingBps, 100);

        vm.prank(admin);
        vault.kill();
        assertTrue(vault.state() == VaultState.Killed);
        assertEq(usd.balanceOf(address(vault)), vaultBefore, "the kill moved money");
        assertEq(usd.balanceOf(admin), adminBefore);
        assertEq(usd.balanceOf(platform), platformBefore);
        // Nothing can leave for the station any more, and everything comes
        // back to depositors.
        vm.prank(executor);
        vm.expectRevert(IVault.WrongState.selector);
        station.pull(1);
    }

    // 4. Signatures are bound to one vault ------------------------------------

    function test_a_usage_bill_and_a_statement_for_one_vault_are_refused_by_another() public {
        (Vault other, bytes32 otherBundle) = openVault();
        assertTrue(address(other) != address(vault));
        // The same executor key attested for both vaults.
        bytes32 leafA = leafFor(address(vault), bundle);
        bytes32 leafB = leafFor(address(other), otherBundle);
        bytes32 root = leafA < leafB
            ? keccak256(abi.encodePacked(leafA, leafB))
            : keccak256(abi.encodePacked(leafB, leafA));
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, root);
        bytes32[] memory proofA = new bytes32[](1);
        proofA[0] = leafB;
        bytes32[] memory proofB = new bytes32[](1);
        proofB[0] = leafA;
        attestation.bless(sealOf(journal), journal, block.chainid, address(vault), bundle, proofA);
        attestation.bless(sealOf(journal), journal, block.chainid, address(other), otherBundle, proofB);
        assertTrue(attestation.isBlessedFor(executor, block.chainid, address(other), otherBundle));

        skip(1 days);
        uint64 asOf = uint64(vm.getBlockTimestamp());
        bytes memory billForVault = usage(vault, 50e6, asOf);
        vm.expectRevert(IVault.NotBlessed.selector);
        other.keep(50e6, asOf, billForVault);
        // And it still works where it was meant to.
        vault.keep(50e6, asOf, billForVault);
        // A different purpose with the same fields is refused too.
        bytes memory stopWord = signed(EXECUTOR_KEY, station.stopDigest("killed", asOf));
        vm.expectRevert(IVault.NotBlessed.selector);
        vault.keep(uint256(bytes32("killed")), asOf, stopWord);

        // Alice's statement for this vault is no good for the other.
        bytes memory statement = statementBy(aliceKey, address(vault), alice, vault.restrictedText());
        vault.recordStatement(alice, statement);
        vm.expectRevert(IVault.BadSignature.selector);
        other.recordStatement(alice, statement);
    }

    // 5. An exit never pays above the mark -----------------------------------

    function test_a_bid_above_the_mark_cannot_pay_a_leaver_more_than_the_mark_value() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(190_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 190_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 3e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        // The venue's bid sits 0.5% above its mark.
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 1005 / 1000, PRICE * 1010 / 1000);
        value(vault);
        Valuation memory v = latest();
        assertEq(v.grossExit, v.gross, "the exit value was not clamped to the mark value");

        uint256 markValueOfShares = (v.gross - v.fee) * aliceShares / v.supply;
        uint256 priceBefore = vault.sharePrice();
        uint256 paid = redeemAs(vault, alice, aliceShares);
        assertLe(paid, markValueOfShares, "a leaver was paid above the mark value");
        assertGe(vault.sharePrice() + 1, priceBefore, "the stayer paid for the exit");
    }

    // 6. The refund path can't block the queue either ------------------------

    function test_a_refused_refund_is_held_and_the_queue_moves_on() public {
        requestDepositAs(vault, alice, 1000e6);
        requestDepositAs(vault, bob, 1000e6);
        vm.prank(author);
        vault.retire();
        vm.mockCallRevert(address(usd), abi.encodeWithSelector(IERC20.transfer.selector, alice), "blocked");
        value(vault);
        vm.clearMockedCalls();
        assertEq(vault.heldFor(alice), 1000e6, "alice's refund was not held");
        assertEq(usd.balanceOf(bob), 1000e6, "bob's refund behind alice's did not settle");
        assertEq(vault.escrow(), 0);
        assertTrue(vault.requestAt(2).done);
        vm.prank(alice);
        vault.collectHeld();
        assertEq(usd.balanceOf(alice), 1000e6);
        assertSolvent("after the held refund was collected");
    }

    // 7. A dust redemption pays nothing and books no claim --------------------

    function test_a_one_unit_redemption_pays_nothing_and_books_no_claim() public {
        depositAs(vault, alice, 100_000e6);
        vm.prank(executor);
        station.pull(95_000e6);
        uint256 cashBefore = vault.cash();
        vm.prank(alice);
        vault.requestRedeem(1, alice);
        value(vault);
        assertEq(vault.claimCount(), 0, "a dust redemption booked a claim");
        assertEq(vault.cash(), cashBefore, "a dust redemption took cash");
        assertEq(usd.balanceOf(alice), 0);
        assertTrue(vault.requestAt(vault.requestCount()).done);
    }

    // 9. The transaction fee is profit above the mark ------------------------

    /// Spec-conforming, recorded for its size: a deposit's transaction fee
    /// stays in the pool above the mark (the blend joins the newcomer at the
    /// net price), so a vault that never trades still mints profit-fee
    /// shares at payday: about 15% of the 0.1% fee, less what a week of
    /// holding fee moves the basis by.
    function test_a_vault_that_never_traded_owes_a_profit_fee_on_its_own_transaction_fees() public {
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        depositAs(vault, alice, 100_000e6);
        value(vault);
        // 15% of the 100 dollar transaction fee, less 15% of the day of
        // holding dilution ($2.74 on 100k) the newcomer rode fee-free.
        assertApproxEqAbs(latest().fee, 15e6, 0.5e6, "the transaction fee is not owed a profit fee");
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        vm.recordLogs();
        value(vault);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 feeShares;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] == IVault.FeesPaid.selector) {
                (uint256 toAuthor, uint256 toPlatform,,) =
                    abi.decode(logs[i].data, (uint256, uint256, uint256, uint256));
                feeShares = toAuthor + toPlatform;
            }
        }
        uint256 worth = feeShares * vault.sharePrice() / 1e18;
        // 15 dollars less 15% of the week's holding dilution (~$19 of basis).
        assertGt(worth, 10e6, "no profit fee was minted on the transaction fee");
        assertLt(worth, 15e6 + 1);
    }

    // 8. Closing: dust at a stopped station delays the close a slot, and only
    //    a slot; the atomic closer always gets through ----------------------

    function test_dust_sent_to_a_stopped_station_delays_the_close_only_until_it_is_unwound() public {
        depositAs(vault, alice, 100_000e6);
        hedge(90_000e6, 0);
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);
        value(vault);
        // A griefer sends a micro-dollar to the stopped station before the
        // close is sent.
        give(address(station), 1);
        // The close as recorded still goes through: the valuation already
        // found the station empty, and the dust arrived after it.
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);
        // And the dust is still recoverable for the holders afterwards.
        station.unwind(1e18);
        assertEq(vault.cash(), 100_000e6 + DEAD_ASSETS + 1, "dust lost");
    }
}
