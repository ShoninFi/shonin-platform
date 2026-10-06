// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LighterVault} from "./LighterVault.sol";

/// The Lighter driver values the station from the newest post, and dollars
/// move between the exchange and the station after it: a withdrawal is
/// carried out into a pending balance which anyone may pay out to the
/// station, and a deposit leaves the station at once and is credited later.
/// The post carries the station's own counters as of its reading (what had
/// reached it from outside its calls, what it had sent the exchange), so
/// each dollar is counted once wherever it is: in the post's figures, in
/// transit, or in the station's own balance.
contract PaidOutWithdrawalCountsOnceTest is LighterVault {
    function test_a_withdrawal_carried_out_and_paid_out_between_two_posts_is_counted_once() public {
        _fundAndTrade();
        (uint256 before,) = station.value();
        assertEq(before, 150_000e6, "the account's worth, by the post");

        // The executor asks for 50k back; the exchange carries it out after
        // the post was made, as it does minutes later.
        vm.prank(executor);
        station.withdraw(LIGHTER, 50_000e6);
        exchange.process();
        (uint256 carriedOut,) = station.value();
        assertEq(carriedOut, before, "carried out after the post: counted once, in the post");

        // Before the next post, a passer-by pays the pending balance out to
        // the station.
        exchange.withdrawPendingBalance(address(station), DOLLAR_ASSET, 50_000e6);
        assertEq(usd.balanceOf(address(station)), 50_000e6, "the dollars reached the station");
        (uint256 paidOut,) = station.value();
        assertEq(paidOut, before, "the same 50k counts once, in the station's dollars");

        // And the next post agrees.
        _nextBlock();
        _post();
        (uint256 reposted,) = station.value();
        assertEq(reposted, before);
    }

    function test_a_leaver_priced_after_a_paid_out_withdrawal_is_paid_the_vaults_true_worth() public {
        uint256 aliceShares = _fundAndTrade();
        vm.prank(executor);
        station.withdraw(LIGHTER, 50_000e6);
        exchange.process();
        exchange.withdrawPendingBalance(address(station), DOLLAR_ASSET, 50_000e6);
        // The post from before the ask prices nothing; the executor's next
        // one, read a settle delay after it, does.
        assertTrue(station.outsideBand(), "a post from before the ask prices nothing");
        skip(lighter.SETTLE_DELAY());
        _post();

        // The vault is worth 200k (50k of cash, 150k at the station). Alice
        // takes a tenth of her shares out, which the cash can pay, priced at
        // the next valuation.
        uint256 shares = aliceShares / 10;
        uint256 paid = redeemAs(vault, alice, shares);
        // A tenth of her 100k, less the transaction fee and the holding fee:
        // within a per cent of 10k.
        assertApproxEqRel(paid, 10_000e6, 0.01e18, "paid at the vault's true worth");
    }

    function test_a_deposit_between_two_posts_neither_vanishes_nor_counts_twice() public {
        _fundAndTrade();
        depositAs(vault, alice, 20_000e6);
        (uint256 before,) = station.value();
        assertEq(before, 150_000e6);

        // The executor sends 10k more; the post before it doesn't know.
        vm.startPrank(executor);
        station.pull(10_000e6);
        station.deposit(LIGHTER, 10_000e6);
        vm.stopPrank();
        (uint256 sent,) = station.value();
        assertEq(sent, 160_000e6, "counted at once, though no post has seen it");
        // A post read before the exchange could have credited it may not
        // show it: such a post prices nothing.
        _nextBlock();
        _post();
        assertTrue(station.outsideBand(), "a post too soon after a deposit prices nothing");
        (uint256 early,) = station.value();
        assertEq(early, 160_000e6, "and still counts it once");

        // Once credited and posted, it is in the post and nowhere else.
        skip(lighter.DEPOSIT_CREDIT());
        _post();
        assertFalse(station.outsideBand());
        (uint256 credited,) = station.value();
        assertEq(credited, 160_000e6, "counted once, in the post");
    }
}
