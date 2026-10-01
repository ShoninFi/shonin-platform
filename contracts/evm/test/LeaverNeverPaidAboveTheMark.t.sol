// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Valuation} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// The exit side is clamped to each station at its mark, so a leaver paid
/// from cash never gets a bid above the mark that nobody sold at, on one
/// chain or two.
contract LeaverNeverPaidAboveTheMarkTest is ClaimBase {
    /// Alice and bob hold half each. 60k of the 200k is one BTC at the
    /// venue; 140k is cash, so alice's whole withdrawal is paid from cash.
    function oneBtcAtTheVenue() internal returns (uint256 aliceShares, uint256 bobShares) {
        aliceShares = depositAs(vault, alice, 100_000e6);
        bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(60_000e6, 60_000e6, 1e18);
        value(vault);
        assertApproxEqAbs(vault.totalValue(), 200_000e6, 1e6);
        assertGe(vault.cash(), 140_000e6);
    }

    /// The spec's clamp: a withdrawal is never priced above the station at
    /// mark. Here the exit side is priced at a bid 10% above the mark.
    function test_the_exit_side_is_clamped_to_the_station_at_mark() public {
        oneBtcAtTheVenue();
        // At the slot the bid leads the mark by 10%; mark and oracle are
        // unchanged at 60k.
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 110 / 100, PRICE * 111 / 100);
        value(vault);
        Valuation memory v = latest();
        emit log_named_uint("gross", v.gross);
        emit log_named_uint("grossExit", v.grossExit);
        assertLe(
            v.grossExit,
            v.cash + 60_000e6 + 1,
            "withdrawals are priced above the station's mark value: the clamp of section 6 does not hold"
        );
    }

    /// A leaver paid at the inflated bid takes more than their shares are
    /// worth, and the holder who stayed is left holding the loss once the
    /// bid is back where it was.
    function test_a_bid_above_the_mark_for_one_block_pays_a_leaver_nothing_extra() public {
        (uint256 aliceShares, uint256 bobShares) = oneBtcAtTheVenue();
        uint256 bobBefore = worth(bobShares);
        uint256 aliceFair = worth(aliceShares);

        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);

        // At the valuation block the bid is lifted 10% above the mark (an
        // attacker buying into the book, or a spoofed bid). One block later
        // it is back.
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 110 / 100, PRICE * 111 / 100);
        value(vault);
        setPrice(SPOT, PRICE, 0);

        uint256 paid = usd.balanceOf(alice);
        emit log_named_uint("alice's shares were worth, at the mark", aliceFair);
        emit log_named_uint("alice was paid", paid);
        emit log_named_uint("bob's worth before", bobBefore);
        emit log_named_uint("bob's worth after", worth(bobShares));

        assertEq(vault.claimCount(), 0, "the withdrawal was not paid from cash");
        assertLe(paid, aliceFair, "a leaver was paid more than their shares were worth at the mark");
        assertGe(worth(bobShares) + 1e6, bobBefore, "the holder who stayed paid for the leaver's exit");
    }
}

/// The same, through a valuation completed by another chain's post.
contract LeaverNeverPaidAboveTheMarkTwoChainTest is TwoChainVault {
    function test_a_two_chain_valuation_pays_a_leaver_at_the_home_mark() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        uint256 bobShares = depositTwo(bob, 100_000e6);

        // One BTC at the home venue, the rest cash.
        vm.startPrank(executor);
        station.pull(60_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 60_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        valueWith(0, 0);

        uint256 bobBefore = worthOf(bobShares);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);

        // The bid leads the mark by 10% in the block the valuation is
        // recorded; the other chain holds nothing; the bid is gone by the
        // time the valuation is closed and priced.
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 110 / 100, PRICE * 111 / 100);
        uint64 time = open();
        setPrice(SPOT, PRICE, 0);
        post(time, postOf(0, 0), EXECUTOR_KEY);
        closeAfterDeadline(time);

        uint256 paid = usd.balanceOf(alice);
        emit log_named_uint("alice was paid", paid);
        emit log_named_uint("bob's worth before", bobBefore);
        emit log_named_uint("bob's worth after", worthOf(bobShares));
        assertLe(paid, 100_000e6, "a leaver was paid more than their shares were worth at the mark");
        assertGe(worthOf(bobShares) + 1e6, bobBefore, "the holder who stayed paid for the leaver's exit");
    }

    /// What `shares` own now at exit prices: cash plus the home station.
    function worthOf(uint256 shares) internal view returns (uint256) {
        (, uint256 exit) = station.value();
        return (vault.cash() + exit) * shares / vault.totalSupply();
    }
}
