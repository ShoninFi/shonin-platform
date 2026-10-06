// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {LighterBoard, LighterReport} from "../../src/drivers/lighter/LighterBoard.sol";
import {LighterDriver} from "../../src/drivers/lighter/LighterDriver.sol";
import {Call} from "../../src/interfaces/IDriver.sol";
import {ClaimState} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";

/// A vault whose station trades on Lighter: the station owns the account,
/// registers the executor's key, and closes and withdraws by priority
/// request; its value is what a blessed key posts on the board.
contract LighterStationTest is LighterVault {
    // The account and the key -------------------------------------------------

    function test_the_first_deposit_makes_the_station_an_account_owner_and_a_second_approval_registers_the_key(
    ) public {
        depositAs(vault, alice, 10_000e6);
        vm.startPrank(executor);
        station.approveKey(LIGHTER, executor);
        assertEq(_master(), 0, "no account before the first deposit");
        station.pull(5000e6);
        station.deposit(LIGHTER, 5000e6);
        // Once the account exists, the executor approves its key again.
        station.approveKey(LIGHTER, executor);
        vm.stopPrank();
        uint48 account = _master();
        assertGt(account, 0);
        assertEq(exchange.ownerOf(account), address(station), "the station owns the account");
        assertEq(
            exchange.pubKey(account, lighter.KEY_SLOT()), EXECUTOR_PUBKEY, "the executor's key is in its slot"
        );
        assertEq(uint256(exchange.cash(account)), 5000e6);
    }

    function test_approving_a_key_with_an_account_puts_it_in_every_account_the_station_owns() public {
        _fundAndTrade();
        (Call[] memory calls) = lighter.approveKey(address(station), executor, 0);
        assertEq(calls.length, 1);
        assertEq(calls[0].target, address(exchange));
    }

    function test_a_key_that_named_no_lighter_key_cannot_be_approved() public {
        address nobody = makeAddr("no key");
        vm.expectRevert(LighterDriver.NoPubKey.selector);
        lighter.approveKey(address(station), nobody, 0);
    }

    function test_revoking_the_key_puts_one_nobody_holds_in_its_slot_and_cancels_its_orders() public {
        _fundAndTrade();
        Call[] memory calls = lighter.revokeKey(address(station), executor);
        assertEq(calls.length, 2);
        vm.prank(address(station));
        (bool ok,) = calls[0].target.call(calls[0].data);
        assertTrue(ok);
        assertEq(exchange.pubKey(_master(), lighter.KEY_SLOT()), lighter.NOBODY());
    }

    // Posts -----------------------------------------------------------------

    function test_only_a_key_blessed_for_the_vault_may_post_its_stations_accounts() public {
        _fundAndTrade();
        LighterReport memory r;
        r.asOf = uint64(block.timestamp + 0);
        vm.prank(makeAddr("stranger"));
        vm.expectRevert(LighterBoard.NotBlessed.selector);
        board.post(address(station), r);
    }

    function test_a_post_no_newer_than_the_last_is_refused() public {
        _fundAndTrade();
        (LighterReport memory r,) = board.reportOf(address(station));
        vm.prank(executor);
        vm.expectRevert(LighterBoard.Stale.selector);
        board.post(address(station), r);
    }

    // Valuing ---------------------------------------------------------------

    function test_the_account_is_valued_at_the_mark_and_the_bid_from_the_newest_post() public {
        _fundAndTrade();
        (uint256 mark, uint256 exit) = lighter.valueOf(address(station));
        assertEq(mark, 150_000e6);
        // 40 ETH, fifty cents under the mark at the bid.
        assertEq(mark - exit, 20e6);
    }

    function test_without_a_fresh_post_every_valuation_is_void() public {
        _fundAndTrade();
        assertFalse(station.outsideBand());
        skip(1 hours + 1);
        assertTrue(station.outsideBand(), "an old post prices nothing");
    }

    function test_a_mark_or_book_outside_the_band_from_the_index_voids_the_valuation() public {
        _fundAndTrade();
        spread = uint32(uint256(ETH_PX) * 2 / 100);
        _nextBlock();
        _post();
        assertTrue(station.outsideBand());
    }

    function test_a_withdrawal_pending_on_chain_is_counted_once_whether_or_not_the_post_has_seen_it()
        public
    {
        _fundAndTrade();
        (uint256 markBefore,) = station.value();
        vm.prank(executor);
        station.withdraw(LIGHTER, 10_000e6);
        exchange.process();
        // The post still shows the 10k in the account; the chain shows it
        // pending too. It is counted once.
        (uint256 mark,) = station.value();
        assertEq(mark, markBefore, "carried out after the post: counted in the post");
        _nextBlock();
        _post();
        (mark,) = station.value();
        assertEq(mark, markBefore, "after a new post: counted as pending");
        exchange.withdrawPendingBalance(address(station), 3, 10_000e6);
        (mark,) = station.value();
        assertEq(mark, markBefore, "paid out: counted in the station's own dollars");
    }

    // A claim's close -------------------------------------------------------

    function test_a_claim_is_closed_by_priority_request_proved_by_a_newer_post_withdrawn_and_paid() public {
        uint256 aliceShares = _fundAndTrade();
        _aliceClaims(aliceShares);

        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Closing, "the claim waits for its own close");
        assertEq(exchange.orderCount(), 1, "one reduce-only order queued");
        (uint48 account, uint16 market, uint48 base, uint32 price, bool isAsk) = exchange.orders(0);
        assertEq(account, _master());
        assertEq(market, ETH);
        assertTrue(isAsk, "a long is closed by selling");
        assertApproxEqRel(uint256(base), 200_000, 0.01e18, "about half");
        assertEq(price, ETH_PX * 99 / 100, "no worse than the band (1%) from the index");

        // Nothing is proved until a newer post shows it.
        _nextBlock();
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();

        exchange.process();
        _post();
        station.settleClose();
        assertEq(exchange.withdrawalCount(), 1, "proved: what it fetched is asked for");
        assertApproxEqRel(station.stage().fetched, 75_000e6, 0.01e18, "her half of the account, measured");

        // Not delivered until the dollars have reached the station.
        _nextBlock();
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
        exchange.process();
        _collect();
        station.settleClose();
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "paid at delivery");
        assertApproxEqRel(usd.balanceOf(alice), 100_000e6, 0.01e18);
        assertApproxEqRel(
            uint256(int256(exchange.size(_master(), ETH))), 200_000, 0.01e18, "Bob's half stays"
        );
    }

    function test_a_stopped_stations_full_unwind_closes_every_market_whole_at_any_price_without_a_post()
        public
    {
        _fundAndTrade();
        skip(2 hours);
        (Call[] memory calls, bool blocked) = lighter.close(address(station), 1e18, 10_000);
        assertFalse(blocked);
        assertEq(calls.length, 2, "both sides of the one market, whole");
    }
}
