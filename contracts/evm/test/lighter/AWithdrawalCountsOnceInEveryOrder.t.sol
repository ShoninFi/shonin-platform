// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LighterVault} from "./LighterVault.sol";

/// A Lighter withdrawal goes through four steps: the station asks for it,
/// the exchange carries it out (out of the account's equity), the
/// exchange's contract credits it to the station's pending balance, and
/// anyone pays it out to the station. A post can be read before, between or
/// after any of them. Whatever the order, the station's worth (its own
/// dollars and the exchange's side by the newest post) counts the
/// withdrawal exactly once at every moment:
///
/// - read before the ask, or before it is carried out: in the post's equity;
/// - read after it is carried out and before the credit: in the post's
///   in-flight dollars;
/// - read after the credit and before the payout: in the post's pending
///   balance;
/// - read after the payout: in the station's dollars, and in the post's own
///   arrivals counter;
///
/// and once paid out after the post, it is in the station's dollars and in
/// the arrivals since the post, which the driver takes off.
contract AWithdrawalCountsOnceInEveryOrderTest is LighterVault {
    uint256 internal constant DEPOSITED = 150_000e6;
    uint256 internal constant WITHDRAWN = 20_000e6;

    function setUp() public override {
        super.setUp();
        exchange.setCreditLater(true);
        // 150k of dollars in the account, no position; nothing else at the
        // station.
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.approveKey(LIGHTER, executor);
        station.pull(DEPOSITED);
        station.deposit(LIGHTER, DEPOSITED);
        station.approveKey(LIGHTER, executor);
        vm.stopPrank();
        skip(lighter.DELIVERY_WINDOW());
        _post();
    }

    function _worth() internal view returns (uint256 mark) {
        (mark,) = station.value();
    }

    function _step(uint256 i) internal {
        if (i == 0) {
            vm.prank(executor);
            station.withdraw(LIGHTER, WITHDRAWN);
        } else if (i == 1) {
            exchange.process();
        } else if (i == 2) {
            exchange.credit();
        } else {
            _collect();
        }
    }

    /// The four steps in order, with a post read after the first `k` of
    /// them, and the station's worth checked after every step and post.
    function _postAfter(uint256 k) internal {
        assertEq(_worth(), DEPOSITED, "before");
        for (uint256 i = 0; i <= 4; i++) {
            if (i == k) {
                skip(1 minutes);
                _post();
                assertEq(_worth(), DEPOSITED, "at the post");
            }
            if (i == 4) break;
            skip(1 minutes);
            _step(i);
            assertEq(_worth(), DEPOSITED, "after a step");
        }
        skip(1 minutes);
        _post();
        assertEq(_worth(), DEPOSITED, "at a post after every step");
        assertEq(usd.balanceOf(address(station)), WITHDRAWN, "the withdrawal is home");
    }

    function test_a_post_read_before_the_ask_counts_it_once_through_every_step() public {
        _postAfter(0);
    }

    function test_a_post_read_after_the_ask_and_before_it_is_carried_out_counts_it_once() public {
        _postAfter(1);
    }

    function test_a_post_read_while_it_is_carried_out_and_not_credited_counts_it_once() public {
        _postAfter(2);
    }

    function test_a_post_read_while_it_is_pending_counts_it_once() public {
        _postAfter(3);
    }

    function test_a_post_read_after_it_is_paid_out_counts_it_once() public {
        _postAfter(4);
    }

    /// After an ask, only a post read a settle delay later prices again: one
    /// read sooner can't tell whether the exchange carried it out.
    function test_only_a_post_read_a_settle_delay_after_an_ask_prices() public {
        assertTrue(lighter.priced(address(station)), "priced before the ask");
        _step(0);
        assertFalse(lighter.priced(address(station)), "the post from before the ask prices nothing");
        skip(lighter.SETTLE_DELAY() - 1);
        _post();
        assertFalse(lighter.priced(address(station)), "a post read too soon after the ask prices nothing");
        skip(1);
        _post();
        assertTrue(lighter.priced(address(station)), "a post read a settle delay after the ask prices");
    }
}
