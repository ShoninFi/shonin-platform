// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {VaultFactory} from "../src/VaultFactory.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ILocalVenue} from "../src/interfaces/ILocalVenue.sol";
import {Allowances} from "../src/interfaces/IPrices.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {
    Claim,
    ClaimState,
    IVault,
    Request,
    Tank,
    Valuation,
    ValuationStatus,
    VaultInit,
    VaultState
} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

contract VaultTest is Fixture {
    address internal constant DEAD = 0x000000000000000000000000000000000000dEaD;
    uint256 internal constant UNWINDER_KEY = 0xD1E;
    bytes32 internal constant UNWINDER_IMAGE = keccak256("unwinder image");

    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    // Helpers ---------------------------------------------------------------

    /// The pool gains (or loses) without anyone moving: the station's money
    /// changes, as a trade's profit would.
    function gain(uint256 amount) internal {
        give(address(station), amount);
    }

    function lose(uint256 amount) internal {
        deal(address(usd), address(station), usd.balanceOf(address(station)) - amount);
    }

    /// Send most of the cash to the station, as the executor does.
    function deploy(uint256 amount) internal {
        vm.prank(executor);
        station.pull(amount);
    }

    /// The executor puts the station's money at the venue, one BTC long
    /// against one short, with a spread of `spreadBps` on spot.
    function hedge(uint256 amount, uint256 spreadBps) internal {
        vm.startPrank(executor);
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

    function nextMondayAfter(uint256 t) internal pure returns (uint256) {
        uint256 day = t / 1 days;
        return (day - ((day + 3) % 7)) * 1 days + 7 days;
    }

    /// Start a fresh fee week: payday has just run, the next is a week away.
    function freshWeek() internal {
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
    }

    /// Skip to the next payday and record a valuation there.
    function toPayday() internal {
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
    }

    function latest() internal view returns (Valuation memory) {
        return vault.valuationAt(vault.valuationCount() - 1);
    }

    function approxEq(uint256 a, uint256 b, uint256 tolerance) internal pure {
        assertApproxEqAbs(a, b, tolerance);
    }

    // Shares ----------------------------------------------------------------

    function test_shares_never_move_between_holders() public {
        depositAs(vault, alice, 1000e6);
        vm.startPrank(alice);
        vm.expectRevert(IVault.NotTransferable.selector);
        vault.transfer(bob, 1);
        vm.expectRevert(IVault.NotTransferable.selector);
        vault.approve(bob, 1);
        vm.expectRevert(IVault.NotTransferable.selector);
        vault.transferFrom(alice, bob, 1);
        vm.stopPrank();
    }

    function test_a_vault_is_initialised_once_and_the_implementation_never() public {
        VaultInit memory init;
        Vault implementation = Vault(factory.vaultImplementation());
        vm.expectRevert(IVault.NotAllowed.selector);
        implementation.initialize(init, 2, 1);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.initialize(init, 2, 1);
    }

    function test_a_donation_to_the_vault_changes_nothing() public {
        depositAs(vault, alice, 1000e6);
        value(vault);
        uint256 before = vault.sharePrice();
        give(address(vault), 1_000_000e6);
        value(vault);
        assertEq(vault.sharePrice(), before);
        assertEq(vault.cash(), 1000e6 + DEAD_ASSETS);
    }

    // Requests and valuations -----------------------------------------------

    function test_a_deposit_is_priced_at_a_later_valuation_never_the_current_one() public {
        // Exactly on a boundary: a request made now waits for the next slot.
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        uint256 id = requestDepositAs(vault, alice, 1000e6);
        vault.settle(10);
        assertEq(vault.balanceOf(alice), 0);
        assertFalse(vault.requestAt(id).done);
        assertEq(vault.escrow(), 1000e6);

        value(vault);
        assertTrue(vault.requestAt(id).done);
        assertGt(vault.balanceOf(alice), 0);
        assertEq(vault.escrow(), 0);
    }

    function test_a_first_deposit_buys_at_a_dollar_less_the_transaction_fee() public {
        uint256 shares = depositAs(vault, alice, 1000e6);
        // 1000 dollars less 0.1%, at 1e12 share units a micro-dollar.
        assertApproxEqRel(shares, 999e6 * 1e12, 0.00001e18);
    }

    function test_a_valuation_counts_only_inside_its_window_and_once_a_slot() public {
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT + 4);
        vm.expectRevert(IVault.OutsideWindow.selector);
        vault.recordValuation();
        vm.warp(t - (t % SLOT) + 2 * SLOT + 2);
        vault.recordValuation();
        vm.expectRevert(IVault.AlreadyValued.selector);
        vault.recordValuation();
    }

    function test_a_new_valuation_waits_for_the_last_ones_requests() public {
        requestDepositAs(vault, alice, 1000e6);
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        vm.warp(vm.getBlockTimestamp() + SLOT);
        vm.expectRevert(IVault.Unsettled.selector);
        vault.recordValuation();
        // Anyone can settle them, and then it can. A deposit is passed twice:
        // once while the slot's exits settle, once to settle it.
        vault.settle(1);
        vm.expectRevert(IVault.Unsettled.selector);
        vault.recordValuation();
        vault.settle(1);
        vault.recordValuation();
    }

    function test_a_deposit_needs_the_depositors_statement_and_the_minimum() public {
        give(alice, 2000e6);
        vm.startPrank(alice);
        usd.approve(address(vault), 2000e6);
        vm.expectRevert(IVault.NoStatement.selector);
        vault.requestDeposit(1000e6, alice, "");
        bytes memory forged = statementBy(bobKey, address(vault), alice, vault.restrictedText());
        vm.expectRevert(IVault.BadSignature.selector);
        vault.requestDeposit(1000e6, alice, forged);
        bytes memory statement = statementBy(aliceKey, address(vault), alice, vault.restrictedText());
        vm.expectRevert(IVault.TooSmall.selector);
        vault.requestDeposit(MINIMUM_DEPOSIT - 1, alice, statement);
        vault.requestDeposit(1000e6, alice, statement);
        // Once recorded, the next deposit needs no signature.
        vault.requestDeposit(MINIMUM_DEPOSIT, alice, "");
        vm.stopPrank();
    }

    /// A receiver the dollar refuses can't stop the queue: their money is
    /// held for them and everyone behind them is settled.
    function test_one_refused_payout_never_blocks_the_queue() public {
        uint256 aliceShares = depositAs(vault, alice, 1000e6);
        depositAs(vault, bob, 1000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        requestDepositAs(vault, bob, 500e6);

        vm.mockCallRevert(address(usd), abi.encodeWithSelector(IERC20.transfer.selector, alice), "blocked");
        value(vault);
        vm.clearMockedCalls();

        uint256 held = vault.heldFor(alice);
        assertGt(held, 0);
        assertTrue(vault.requestAt(vault.requestCount()).done, "the request behind the refused one settled");
        vm.prank(alice);
        vault.collectHeld();
        assertEq(usd.balanceOf(alice), held);
    }

    function test_a_deposit_made_before_a_retirement_is_sent_back() public {
        requestDepositAs(vault, alice, 1000e6);
        vm.prank(author);
        vault.retire();
        value(vault);
        assertEq(vault.balanceOf(alice), 0);
        assertEq(usd.balanceOf(alice), 1000e6);
        assertEq(vault.escrow(), 0);
    }

    // Fees ------------------------------------------------------------------

    function test_the_holding_fee_dilutes_the_price_and_is_handed_over_on_payday() public {
        depositAs(vault, alice, 100_000e6);
        freshWeek();
        uint256 before = vault.sharePrice();
        skip(73 days / 20);
        value(vault);
        // A hundredth of a year at 1%: a ten-thousandth lower, the shares
        // held by the vault until payday.
        approxEq(vault.sharePrice(), before - before / 10_000, before / 100_000);
        assertGt(vault.balanceOf(address(vault)), 0);
        toPayday();
        assertEq(vault.balanceOf(address(vault)), 0);
        assertGt(vault.balanceOf(platform), 0);
    }

    function test_the_profit_fee_is_owed_all_week_and_paid_on_monday_without_moving_the_price() public {
        depositAs(vault, alice, 100_000e6);
        freshWeek();
        skip(3 days);
        value(vault);
        gain(10_000e6);
        value(vault);
        Valuation memory owedThen = latest();
        // 15% of the gain is owed, and the price is shown after it.
        approxEq(owedThen.fee, 1500e6, 5e6);
        uint256 authorBefore = vault.balanceOf(author);
        uint256 platformBefore = vault.balanceOf(platform);
        uint256 priceBefore = vault.sharePrice();

        toPayday();
        uint256 toAuthor = vault.balanceOf(author) - authorBefore;
        uint256 toPlatform = vault.balanceOf(platform) - platformBefore;
        assertEq(latest().fee, 0);
        approxEq(vault.sharePrice(), priceBefore, priceBefore / 10_000);
        // 1500 dollars of fee: two thirds to the author, a third (and the
        // holding fee) to the platform.
        assertApproxEqRel(toAuthor * vault.sharePrice() / 1e18, 1000e6, 0.01e18);
        assertGt(toPlatform * 2, toAuthor);
    }

    function test_a_gain_given_back_before_payday_mints_nothing() public {
        depositAs(vault, alice, 100_000e6);
        freshWeek();
        skip(3 days);
        value(vault);
        gain(10_000e6);
        value(vault);
        assertGt(latest().fee, 0);
        uint256 authorBefore = vault.balanceOf(author);
        lose(10_000e6);
        toPayday();
        assertEq(vault.balanceOf(author), authorBefore);
    }

    /// Money joining above the mark joins at its own price: nobody's price
    /// moves, and the fee owed doesn't jump.
    function test_a_deposit_above_the_mark_blends_it() public {
        depositAs(vault, alice, 100_000e6);
        gain(20_000e6);
        value(vault);
        uint256 priceBefore = vault.sharePrice();
        uint256 feeBefore = latest().fee;
        uint256 markBefore = vault.mark();

        depositAs(vault, bob, 100_000e6);
        assertGt(vault.mark(), markBefore);
        approxEq(vault.sharePrice(), priceBefore, priceBefore / 10_000);
        approxEq(latest().fee, feeBefore, feeBefore / 1000 + 1);
    }

    function test_a_deposit_below_the_mark_leaves_it_and_rides_the_recovery_free() public {
        depositAs(vault, alice, 100_000e6);
        deploy(50_000e6);
        lose(10_000e6);
        value(vault);
        uint256 markBefore = vault.mark();
        depositAs(vault, bob, 100_000e6);
        assertEq(vault.mark(), markBefore);
    }

    /// A leaver pays the fee their own shares owe, as a slice of those
    /// shares, and everyone who stayed keeps their price.
    function test_a_withdrawal_pays_its_own_fee_and_moves_nobody_elses_price() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        gain(20_000e6);
        value(vault);
        uint256 priceBefore = vault.sharePrice();

        uint256 paid = redeemAs(vault, alice, aliceShares);
        // Half of 220k, less 15% of half the 20k gain, less 0.1%.
        approxEq(paid, (110_000e6 - 1500e6) * 999 / 1000, 50e6);
        assertGt(vault.balanceOf(author), 0, "the slice went to the author");
        assertGe(vault.sharePrice(), priceBefore - priceBefore / 100_000);
    }

    function test_a_withdrawal_is_paid_at_the_exit_side_and_the_spread_stays_with_the_pool() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploy(120_000e6);
        hedge(120_000e6, 200);
        value(vault);
        Valuation memory v = latest();
        assertLt(v.grossExit, v.gross);
        uint256 priceBefore = vault.sharePrice();

        uint256 paid = redeemAs(vault, alice, aliceShares);
        assertLt(paid, 100_000e6 - 100e6);
        assertGe(vault.sharePrice(), priceBefore);
    }

    // The tank --------------------------------------------------------------

    function test_keep_charges_the_bill_up_to_the_allowances_for_the_time_elapsed() public {
        skip(1 days);
        uint64 asOf = uint64(vm.getBlockTimestamp());
        uint256 before = usd.balanceOf(platform);
        // Asked for 50 dollars; the allowances cost 2 a day.
        vault.keep(50e6, asOf, usage(vault, 50e6, asOf));
        approxEq(usd.balanceOf(platform) - before, 2e6, 1);
        Tank memory t = vault.tank();
        assertEq(t.usageSettled, 50e6);
        // The rest is written off: no debt, and the same bill can't be
        // collected twice.
        bytes memory again = usage(vault, 50e6, asOf);
        vm.expectRevert(IVault.Stale.selector);
        vault.keep(50e6, asOf, again);
        approxEq(usd.balanceOf(platform) - before, 2e6, 1);
    }

    function test_a_block_of_bills_cannot_charge_more_than_the_time_elapsed() public {
        uint64 asOf = uint64(vm.getBlockTimestamp());
        uint256 before = usd.balanceOf(platform);
        vault.keep(2e6, asOf, usage(vault, 2e6, asOf));
        // Every later bill in the same moment is refused: each must be signed
        // later than the last.
        for (uint256 i = 2; i <= 30; i++) {
            bytes memory bill = usage(vault, i * 2e6, asOf);
            vm.expectRevert(IVault.Stale.selector);
            vault.keep(i * 2e6, asOf, bill);
        }
        assertEq(usd.balanceOf(platform), before, "no time passed, so nothing was earned");
    }

    function test_the_bill_stops_at_the_halt() public {
        vm.prank(admin);
        vault.kill();
        skip(30 days);
        blessExecutorKey(executor, address(vault), bundle);
        uint64 asOf = uint64(vm.getBlockTimestamp());
        uint256 before = usd.balanceOf(platform);
        vault.keep(100e6, asOf, usage(vault, 100e6, asOf));
        // Ten minutes of grace at 2 dollars a day.
        assertLe(usd.balanceOf(platform) - before, 2e6 * HALT_GRACE / 1 days + 1);
    }

    function test_only_the_executor_bills_and_a_total_never_goes_backwards() public {
        vm.prank(admin);
        attestation.proposeImage(UNWINDER_IMAGE, ImageKind.Unwinder);
        skip(TIMELOCK);
        blessServiceKey(BUILDER_IMAGE, builder);
        blessExecutorKey(executor, address(vault), bundle);
        address unwinder = vm.addr(UNWINDER_KEY);
        bytes memory journal = journalFor(UNWINDER_IMAGE, unwinder, leafFor(address(vault), bundle));
        attestation.bless(sealOf(journal), journal, block.chainid, address(vault), bundle, new bytes32[](0));

        uint64 asOf = uint64(vm.getBlockTimestamp());
        bytes memory byUnwinder = signed(UNWINDER_KEY, vault.usageDigest(2e6, asOf));
        vm.expectRevert(IVault.NotBlessed.selector);
        vault.keep(2e6, asOf, byUnwinder);

        vault.keep(2e6, asOf, usage(vault, 2e6, asOf));
        bytes memory older = usage(vault, 1e6, asOf);
        vm.expectRevert(IVault.Stale.selector);
        vault.keep(1e6, asOf, older);
    }

    function test_the_author_withdraws_the_tank_after_a_fresh_bill() public {
        skip(1 days);
        uint64 asOf = uint64(vm.getBlockTimestamp());
        bytes memory bill = usage(vault, 1e6, asOf);
        vm.prank(author);
        vault.withdrawTank(10e6, 1e6, asOf, bill);
        assertEq(usd.balanceOf(author), 10e6);

        skip(1 hours);
        bytes memory stale = usage(vault, 2e6, asOf + 1);
        vm.prank(author);
        vm.expectRevert(IVault.Stale.selector);
        vault.withdrawTank(10e6, 2e6, asOf + 1, stale);
    }

    function test_with_no_executor_the_author_withdraws_after_a_day() public {
        vm.startPrank(author);
        vm.expectRevert(IVault.NotYet.selector);
        vault.withdrawTankUnsigned(10e6);
        vault.requestTankWithdrawal();
        skip(1 days - 1);
        vm.expectRevert(IVault.NotYet.selector);
        vault.withdrawTankUnsigned(10e6);
        skip(1);
        vault.withdrawTankUnsigned(10e6);
        vm.stopPrank();
        assertEq(usd.balanceOf(author), 10e6);
    }

    function test_auto_fill_redeems_the_authors_shares_like_any_withdrawal() public {
        depositAs(vault, alice, 100_000e6);
        freshWeek();
        skip(3 days);
        value(vault);
        gain(20_000e6);
        toPayday();
        uint256 authorShares = vault.balanceOf(author);
        assertGt(authorShares, 0);

        vm.prank(author);
        vault.setTankKeep(100e6);
        vault.keep(0, 0, "");
        uint256 priceBefore = vault.sharePrice();
        value(vault);
        Tank memory t = vault.tank();
        approxEq(t.balance, 100e6, 1e6);
        assertLt(vault.balanceOf(author), authorShares);
        // Priced at the exit side with the transaction fee: nobody else pays.
        assertGe(vault.sharePrice(), priceBefore - 1);
    }

    function test_a_fill_never_becomes_a_claim() public {
        vm.prank(author);
        vault.setTankKeep(1000e6);
        vault.keep(0, 0, "");
        value(vault);
        assertEq(vault.claimCount(), 0);
    }

    function test_a_change_of_allowances_closes_the_cap_at_the_old_rate() public {
        skip(1 days);
        Allowances memory dearer = allowancesCosting(10e6);
        vm.prank(author);
        vault.setAllowances(dearer);
        skip(1 days);
        uint64 asOf = uint64(vm.getBlockTimestamp());
        uint256 before = usd.balanceOf(platform);
        vault.keep(100e6, asOf, usage(vault, 100e6, asOf));
        approxEq(usd.balanceOf(platform) - before, 12e6, 2);
    }

    // Claims and the fallback -----------------------------------------------

    function bookClaim() internal returns (uint256 claimId, uint256 shares) {
        shares = depositAs(vault, alice, 100_000e6);
        deploy(90_000e6);
        hedge(90_000e6, 0);
        vm.prank(alice);
        vault.requestRedeem(shares, alice);
        value(vault);
        claimId = vault.claimCount();
        assertEq(claimId, 1);
    }

    function test_a_withdrawal_the_cash_cant_pay_becomes_a_claim_and_is_paid_at_its_close() public {
        (uint256 claimId, uint256 shares) = bookClaim();
        assertTrue(vault.claimAt(claimId).state == ClaimState.Pending);
        assertEq(vault.balanceOf(address(vault)), shares + vault.balanceOf(address(vault)) - shares);

        vault.settleClaim(claimId);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.settleClaim(claimId);

        Claim memory c = vault.claimAt(claimId);
        assertTrue(c.state == ClaimState.Paid);
        approxEq(c.paid, 100_000e6 * 999 / 1000, 300e6);
        assertEq(usd.balanceOf(alice), c.paid);
    }

    function test_a_claim_waits_while_the_market_is_outside_the_band() public {
        (uint256 claimId,) = bookClaim();
        setPrice(SPOT, PRICE, 600);
        vault.settleClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Waiting);
    }

    function test_a_claim_unpaid_for_a_week_freezes_the_vault_whatever_was_tried() public {
        (uint256 claimId,) = bookClaim();
        setPrice(SPOT, PRICE, 600);
        skip(6 days);
        vault.settleClaim(claimId);
        vm.expectRevert(IVault.NotYet.selector);
        vault.freeze(claimId);
        skip(1 days);
        vault.freeze(claimId);
        assertTrue(vault.state() == VaultState.Frozen);
        assertGt(vault.haltedAt(), 0);
    }

    function test_with_no_valuation_for_a_day_a_leaver_takes_the_cash_share_and_units() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploy(150_000e6);
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);

        vm.prank(alice);
        vm.expectRevert(IVault.NotYet.selector);
        vault.takeProRata(id);
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRata(id);

        // About half of the 50k left in cash, and units for the rest.
        approxEq(usd.balanceOf(alice), 25_000e6, 50e6);
        // Her units are her fraction of the station: about half of it.
        (uint256 held,) = vault.unitsOf(alice);
        assertApproxEqRel(held, 0.5e18, 0.01e18);

        // Nothing goes out to the station while units are owed.
        vm.prank(executor);
        vm.expectRevert(IVault.WrongState.selector);
        station.pull(1);

        // Money coming home is split: alice's part to her units.
        vm.prank(executor);
        station.push(150_000e6);
        vm.prank(alice);
        vault.collectUnits();
        approxEq(usd.balanceOf(alice), 100_000e6, 200e6);
    }

    function test_serving_a_unit_holder_closes_exactly_their_fraction() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploy(150_000e6);
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRata(id);

        vault.serveProRata(alice);
        vm.prank(alice);
        vault.collectUnits();
        approxEq(usd.balanceOf(alice), 100_000e6, 200e6);
        (uint256 total,) = vault.units();
        assertEq(total, 0);
    }

    // Lifecycle -------------------------------------------------------------

    function test_retire_stops_deposits_and_evict_halts_after_the_wind_down() public {
        vm.prank(alice);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.retire();
        vm.prank(author);
        vault.retire();
        assertTrue(vault.state() == VaultState.WindingDown);
        bytes memory statement = statementBy(aliceKey, address(vault), alice, vault.restrictedText());
        give(alice, 100e6);
        vm.startPrank(alice);
        usd.approve(address(vault), 100e6);
        vm.expectRevert(IVault.WrongState.selector);
        vault.requestDeposit(100e6, alice, statement);
        vm.stopPrank();

        vm.expectRevert(IVault.NotYet.selector);
        vault.evict();
        skip(7 days);
        vault.evict();
        assertGt(vault.haltedAt(), 0);
    }

    function test_only_the_admin_kills() public {
        vm.prank(alice);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.kill();
        vm.prank(admin);
        vault.kill();
        assertTrue(vault.state() == VaultState.Killed);
    }

    /// A killed vault: the agent keeps its keys for the grace, then anyone
    /// stops the station and unwinds it, the vault closes once a valuation
    /// shows the station empty, and everyone takes their share of the cash.
    function test_a_killed_vault_unwinds_closes_and_pays_everyone() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 50_000e6);
        deploy(120_000e6);
        hedge(120_000e6, 0);

        vm.prank(admin);
        vault.kill();
        vm.expectRevert(Station.NotHalted.selector);
        station.stop();
        skip(HALT_GRACE);
        station.stop();
        vm.prank(alice);
        station.unwind(1e18);
        assertEq(vault.cash(), 150_000e6 + DEAD_ASSETS);

        vm.expectRevert(IVault.NotYet.selector);
        vault.close();
        value(vault);
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);

        vm.prank(alice);
        uint256 toAlice = vault.redeemClosed(aliceShares, alice);
        vm.prank(bob);
        uint256 toBob = vault.redeemClosed(bobShares, bob);
        approxEq(toAlice, 100_000e6, 200e6);
        approxEq(toBob, 50_000e6, 100e6);
    }

    function test_a_strategy_trading_on_another_chain_cannot_be_activated_yet() public {
        (uint256 id,) = readyOnTwoChains();
        (, string memory text) = factory.restrictedFor(id);
        bytes memory statement = statementBy(authorKey, factory.predictVault(id), author, text);
        bytes32 terms = factory.terms();
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        vm.expectRevert(VaultFactory.OtherChainsNotYet.selector);
        factory.activate(id, allowancesCosting(2e6), 500, MINIMUM_TANK, 0, statement, terms);
        vm.stopPrank();
    }

    function readyOnTwoChains() internal returns (uint256 id, bytes32 b) {
        id = submitAs(author);
        b = keccak256(abi.encode("bundle", id));
        bytes32[] memory none = new bytes32[](0);
        bytes32[] memory restricted = new bytes32[](2);
        restricted[0] = "SANCTIONED";
        restricted[1] = "US";
        bytes32[] memory bridges = new bytes32[](1);
        bridges[0] = "local-bridge";
        bytes32 digest = strategies.registrationDigest(
            id, author, keccak256("source"), b, keccak256("manifest"), twoChains(), bridges, restricted
        );
        strategies.register(
            id,
            author,
            keccak256("source"),
            b,
            keccak256("manifest"),
            twoChains(),
            bridges,
            restricted,
            signed(BUILDER_KEY, digest),
            signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")))
        );
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(b, 1));
        none;
    }

    function twoChains() internal pure returns (ChainNeed[] memory chains) {
        chains = new ChainNeed[](2);
        chains[0].chain = "base";
        chains[0].drivers = new bytes32[](1);
        chains[0].drivers[0] = "local";
        chains[1].chain = "hyperliquid";
        chains[1].drivers = new bytes32[](1);
        chains[1].drivers[0] = "local";
    }

    // Who may deposit -------------------------------------------------------

    function test_a_longer_list_needs_a_new_statement_and_a_pending_deposit_goes_back() public {
        depositAs(vault, alice, 1000e6);
        requestDepositAs(vault, alice, 1000e6);

        // The driver's terms now exclude another country.
        RestrictiveDriver stricter = new RestrictiveDriver(venue, IERC20(address(usd)));
        vm.prank(admin);
        attestation.proposeDriver("local", address(stricter));
        skip(TIMELOCK);
        vault.refreshRestricted();
        assertEq(vault.restrictedText(), "KP,SANCTIONED,US");

        value(vault);
        assertEq(usd.balanceOf(alice), 1000e6, "the pending deposit went back");

        give(alice, 100e6);
        vm.startPrank(alice);
        usd.approve(address(vault), 100e6);
        vm.expectRevert(IVault.NoStatement.selector);
        vault.requestDeposit(100e6, alice, "");
        vm.stopPrank();
        requestDepositAs(vault, alice, 100e6);
    }

    // The idle floor --------------------------------------------------------

    function test_the_station_cant_take_cash_below_the_idle_floor() public {
        depositAs(vault, alice, 100_000e6);
        // A 5% floor of about 100k.
        vm.prank(executor);
        vm.expectRevert(IVault.BelowFloor.selector);
        station.pull(96_000e6);
        deploy(94_000e6);
    }

    function test_lowering_the_floor_waits_a_day_and_raising_it_doesnt() public {
        depositAs(vault, alice, 100_000e6);
        vm.prank(author);
        vault.setIdleFloor(0);
        vm.prank(executor);
        vm.expectRevert(IVault.BelowFloor.selector);
        station.pull(96_000e6);
        skip(1 days);
        value(vault);
        deploy(96_000e6);

        vm.prank(author);
        vault.setIdleFloor(9000);
        (uint16 bps,,) = vault.idleFloor();
        assertEq(bps, 9000);
    }
}

/// The local driver, whose venue's terms now also exclude North Korea.
contract RestrictiveDriver is IDriver {
    LocalDriver internal immutable inner;

    constructor(ILocalVenue venue_, IERC20 asset_) {
        inner = new LocalDriver(venue_, asset_);
    }

    function name() external pure returns (bytes32) {
        return "local";
    }

    function targets() external view returns (address[] memory) {
        return inner.targets();
    }

    function restricted() external pure returns (bytes32[] memory list) {
        list = new bytes32[](1);
        list[0] = "KP";
    }

    function custody() external pure returns (Custody) {
        return Custody.Venue;
    }

    function exitPath() external pure returns (ExitPath) {
        return ExitPath.Contract;
    }

    function valuation() external pure returns (ValuationKind) {
        return ValuationKind.OnChain;
    }

    function approveKey(address station, address key, uint64 until) external view returns (Call[] memory) {
        return inner.approveKey(station, key, until);
    }

    function revokeKey(address station, address key) external view returns (Call[] memory) {
        return inner.revokeKey(station, key);
    }

    function deposit(address station, uint256 amount) external view returns (Call[] memory) {
        return inner.deposit(station, amount);
    }

    function withdraw(address station, uint256 amount) external view returns (Call[] memory) {
        return inner.withdraw(station, amount);
    }

    function close(
        address station,
        uint256 fraction,
        uint16 band
    ) external view returns (Call[] memory, bool) {
        return inner.close(station, fraction, band);
    }

    function sweep(address station, uint256 fraction) external view returns (Call[] memory) {
        return inner.sweep(station, fraction);
    }

    function valueOf(address station) external view returns (uint256, uint256) {
        return inner.valueOf(station);
    }
}
