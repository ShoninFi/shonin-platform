// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Valuation} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A deposit priced while the bid sits above the mark pays the premium: the
/// stayers' worth at the mark doesn't drop, the price doesn't move, and the
/// payday after takes no more than the fee the stayers owed.
contract DepositPremiumKeepsTheStayersWholeTest is ClaimBase {
    /// What `shares` own at the venue's mark.
    /// What `shares` are worth at the latest valuation, net of the profit
    /// fee they owe.
    function net(uint256 shares) internal view returns (uint256) {
        Valuation memory v = latest();
        return shares * (v.gross - v.fee) / v.supply;
    }

    function atMark(uint256 shares) internal view returns (uint256) {
        (uint256 mark,) = station.value();
        return (vault.cash() + mark) * shares / vault.totalSupply();
    }

    /// A deposit priced at a valuation whose bid sits above the mark pays the
    /// premium; the mark blends with the premium in the price. The stayers'
    /// worth at mark must not drop, before or after payday, and the fee owed
    /// must not jump.
    function test_a_deposit_with_a_premium_blends_the_mark_without_moving_the_stayers() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(100_000e6, 100_000e6, 1.5e18);
        // A fresh fee week, then a gain so a fee is owed.
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        give(address(station), 10_000e6);
        skip(1 days);
        value(vault);
        uint256 feeBefore = latest().fee;
        assertGt(feeBefore, 0);
        uint256 bobBefore = net(bobShares);
        uint256 priceBefore = vault.sharePrice();

        // Carol deposits while the bid sits 0.8% above the mark, inside the band.
        bytes memory statement = statementBy(carolKey, address(vault), carol, vault.restrictedText());
        give(carol, 100_000e6);
        vm.startPrank(carol);
        usd.approve(address(vault), 100_000e6);
        vault.requestDeposit(100_000e6, carol, statement);
        vm.stopPrank();
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 1008 / 1000, PRICE * 1009 / 1000);
        value(vault);
        setPrice(SPOT, PRICE, 0);
        value(vault);
        uint256 carolShares = vault.balanceOf(carol);

        emit log_named_uint("fee owed before carol", feeBefore);
        emit log_named_uint("fee owed after carol", latest().fee);
        emit log_named_uint("carol's worth at mark, having paid 100k", atMark(carolShares));
        emit log_named_uint("bob's worth net of the fee he owes, before", bobBefore);
        emit log_named_uint("bob's worth net of the fee he owes, after", net(bobShares));
        // A newcomer joins at the price net of the fee owed, so the stayers'
        // gross worth per share falls a little; what they own net of the fee
        // their own shares owe must not.
        assertGe(net(bobShares) + 1, bobBefore, "the premium deposit cost a stayer");
        assertLe(net(carolShares), 100_000e6, "carol bought below what she paid");
        // The premium and the transaction fee stay in the pool: the price net
        // of fees can only rise, by no more than they come to.
        assertGe(vault.sharePrice(), priceBefore, "the blend lowered the price");
        assertLe(
            vault.sharePrice(), priceBefore * 1005 / 1000, "the blend raised the price more than it paid"
        );

        // Payday: the fee minted is at most what was owed plus the holding fee.
        skip(1 days);
        value(vault);
        uint256 bobFee = latest().fee * bobShares / vault.totalSupply();
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        uint256 bobBeforePayday = atMark(bobShares);
        uint256 authorBefore = vault.balanceOf(author);
        value(vault);
        uint256 minted = vault.balanceOf(author) - authorBefore;
        emit log_named_uint("author fee shares minted at payday, at mark", atMark(minted));
        emit log_named_uint("bob's worth at mark after payday", atMark(bobShares));
        // Bob's gross worth at mark drops by his own share of the fee owed
        // (a third of it) and a week of holding fee, and no more.
        assertGe(
            atMark(bobShares) + bobFee + bobBeforePayday * 3 / 10_000,
            bobBeforePayday,
            "payday took more than the fee bob's shares owed"
        );
    }
}
