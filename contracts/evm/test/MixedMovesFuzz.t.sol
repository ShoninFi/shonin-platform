// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Under fuzzed gains, in-band spreads, bid premiums, deposit sizes,
/// partial exits, paydays and a leaver-timed claim, the holders who stay
/// keep their net price and no round trip returns more than went in.
contract MixedMovesFuzzTest is ClaimBase {
    /// Worth per share at the mark side, net of the profit fee owed, times
    /// 1e18.
    function netPerShare() internal view returns (uint256) {
        (uint256 markValue,) = station.value();
        uint256 total = vault.cash() + markValue + 1;
        uint256 supply = vault.totalSupply() + 1e12;
        uint256 basis = ceilDiv(vault.mark() * supply, 1e30);
        uint256 fee = total > basis ? (total - basis) * 1500 / 10_000 : 0;
        return (total - fee) * 1e18 / supply;
    }

    function setMarket(uint256 price, uint256 spreadBps, uint256 premiumBps) internal {
        // The bid sits `premiumBps` above the mark, or `spreadBps / 2` below it.
        uint256 bid =
            premiumBps != 0 ? price * (10_000 + premiumBps) / 10_000 : price - price * spreadBps / 20_000;
        uint256 ask = bid + price * spreadBps / 20_000 + 1;
        vm.prank(operator);
        venue.setPrices(SPOT, price, price, bid, ask);
    }

    function testFuzz_stayers_keep_their_price_through_a_mixed_slot(
        uint256 gainBps,
        uint256 spreadBps,
        uint256 premiumBps,
        uint256 deposit,
        uint256 leaveBps,
        bool payday
    ) public {
        gainBps = bound(gainBps, 0, 5000);
        spreadBps = bound(spreadBps, 0, 180); // half of it is inside the 1% band
        premiumBps = bound(premiumBps, 0, 300);
        deposit = bound(deposit, MINIMUM_DEPOSIT, 500_000e6);
        leaveBps = bound(leaveBps, 1, 10_000);

        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(150_000e6, 150_000e6, 2e18);
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        skip(1 days);

        uint256 price = PRICE * (10_000 + gainBps) / 10_000;
        setMarket(price, 0, 0);
        value(vault);

        // Carol joins and alice leaves part of her holding at one slot.
        uint256 aliceLeaves = aliceShares * leaveBps / 10_000;
        vm.prank(alice);
        vault.requestRedeem(aliceLeaves, alice);
        bytes memory statement = statementBy(carolKey, address(vault), carol, vault.restrictedText());
        give(carol, deposit);
        vm.startPrank(carol);
        usd.approve(address(vault), deposit);
        vault.requestDeposit(deposit, carol, statement);
        vm.stopPrank();

        if (payday) vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        setMarket(price, spreadBps, premiumBps);
        uint256 bobBefore = netPerShare();
        uint256 aliceFair = worth(aliceLeaves);
        value(vault);
        uint256 bobAfter = netPerShare();
        // A payday on the slot is a week of holding fee at most; otherwise
        // only rounding.
        uint256 slack = payday ? bobBefore * 3 / 10_000 : 2;
        assertGe(bobAfter + slack, bobBefore, "the stayer lost on the slot");

        uint256 alicePaid = usd.balanceOf(alice);
        if (vault.claimCount() == 0) {
            assertLe(alicePaid, aliceFair + 1, "a leaver was paid above her fraction at the exit side");
        } else {
            // Cash short: she closes her fraction when she likes.
            setMarket(price, spreadBps, 0);
            uint256 stayer = netPerShare();
            uint256 fair = worth(aliceLeaves);
            vault.settleClaim(1);
            if (vault.claimAt(1).state == ClaimState.Paid) {
                assertLe(vault.claimAt(1).paid, fair + 1, "a closed claim was paid above its fraction");
                assertGe(netPerShare() + 2, stayer, "the stayer lost on the close");
            }
        }

        // Carol's round trip at the same market returns no more than went in.
        uint256 carolShares = vault.balanceOf(carol);
        if (carolShares != 0) {
            setMarket(price, spreadBps, premiumBps);
            uint256 before = netPerShare();
            uint256 back = redeemAs(vault, carol, carolShares);
            if (vault.claimCount() == 0 || vault.claimAt(vault.claimCount()).owner != carol) {
                assertLe(back, deposit, "a round trip returned more than went in");
            }
            assertGe(netPerShare() + 2, before, "the stayer lost on the round trip");
        }
        bobShares;
    }
}
