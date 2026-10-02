// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Valuation} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Every deposit priced at one valuation pays the same price, whatever its
/// place in the queue, including when the exit side sits above the mark:
/// the fee a deposit is priced net of is fixed when the valuation is priced.
contract DepositsInOneSlotPayOnePriceTest is ClaimBase {
    /// A pool well above its mark, a week into its fee period: alice and bob
    /// hold half each, 3 BTC at the venue, BTC up 20% since payday.
    function aboveTheMark() internal {
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 3e18);
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault); // payday: the mark catches up to the price
        skip(1 days);
        setPrice(SPOT, PRICE * 120 / 100, 0);
        value(vault);
        assertGt(latest().fee, 0, "a fee is owed above the mark");
    }

    /// The venue's bid one unit above its mark at the next valuation, so the
    /// valuation carries `extra > 0` and `_deposit` takes the live-mark path.
    function bidAHairAboveTheMark() internal {
        uint256 p = PRICE * 120 / 100;
        vm.prank(operator);
        venue.setPrices(SPOT, p, p, p + 1, p + 2);
    }

    function requestDeposit(address who, uint256 key, uint256 amount) internal {
        bytes memory statement = statementBy(key, address(vault), who, vault.restrictedText());
        give(who, amount);
        vm.startPrank(who);
        usd.approve(address(vault), amount);
        vault.requestDeposit(amount, who, statement);
        vm.stopPrank();
    }

    function test_the_same_valuation_prices_two_deposits_alike_when_the_exit_side_leads() public {
        aboveTheMark();
        bidAHairAboveTheMark();

        // Carol alone: the price she pays at this valuation.
        uint256 snap = vm.snapshotState();
        requestDeposit(carol, carolKey, 100_000e6);
        value(vault);
        uint256 carolAlone = vault.balanceOf(carol);
        Valuation memory vAlone = latest();
        vm.revertToState(snap);

        // Alice's deposit lands in the queue ahead of carol's; both are
        // priced at the one valuation.
        requestDeposit(alice, aliceKey, 100_000e6);
        requestDeposit(carol, carolKey, 100_000e6);
        uint256 aliceBefore = vault.balanceOf(alice);
        value(vault);
        uint256 aliceGot = vault.balanceOf(alice) - aliceBefore;
        uint256 carolBehind = vault.balanceOf(carol);
        Valuation memory vBoth = latest();

        // Same snapshot in both runs.
        assertEq(vBoth.gross, vAlone.gross);
        assertEq(vBoth.supply, vAlone.supply);
        assertEq(vBoth.fee, vAlone.fee);

        emit log_named_uint("shares carol gets for 100k, alone in the slot", carolAlone);
        emit log_named_uint("shares carol gets for 100k, behind alice      ", carolBehind);
        emit log_named_uint("shares alice gets for 100k, ahead of carol    ", aliceGot);
        emit log_named_uint("carol's shortfall, in shares", carolAlone - carolBehind);
        emit log_named_uint(
            "carol's shortfall, in bps of her deposit", (carolAlone - carolBehind) * 10_000 / carolAlone
        );

        // One slot, one price: the same 100k at the same valuation must buy
        // the same shares, whoever is ahead in the queue (within rounding).
        assertApproxEqAbs(carolBehind, carolAlone, 1e12, "the queue order changed carol's price");
        assertApproxEqAbs(
            carolBehind, aliceGot, 1e12, "two equal deposits at one valuation bought different shares"
        );
    }

    /// The same two deposits with `extra == 0` are priced alike: the defect
    /// is only on the `extra != 0` branch.
    function test_with_no_extra_the_two_deposits_are_priced_alike() public {
        aboveTheMark();
        requestDeposit(alice, aliceKey, 100_000e6);
        requestDeposit(carol, carolKey, 100_000e6);
        uint256 aliceBefore = vault.balanceOf(alice);
        value(vault);
        uint256 aliceGot = vault.balanceOf(alice) - aliceBefore;
        assertApproxEqAbs(vault.balanceOf(carol), aliceGot, 1e12, "priced differently even without extra");
    }
}
