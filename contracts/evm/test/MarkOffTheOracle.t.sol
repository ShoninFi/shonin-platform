// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Market} from "../src/interfaces/ILocalVenue.sol";
import {Valuation, ValuationStatus} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A valuation at which a held market's mark, bid or ask sits further from
/// the venue's own price than the band, on either side, prices nothing: no
/// leaver is paid, no depositor buys and no fee is minted at a mark nobody
/// trusts, and the requests are priced at the next valuation.
contract MarkOffTheOracleTest is ClaimBase {
    /// Alice and bob hold half each. One BTC (60k) sits at the venue; 140k
    /// is cash, so a leaver's whole withdrawal is paid from cash.
    function halfAtTheVenue() internal returns (uint256 aliceShares, uint256 bobShares) {
        aliceShares = depositAs(vault, alice, 100_000e6);
        bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(60_000e6, 60_000e6, 1e18);
        value(vault);
        assertApproxEqAbs(vault.totalValue(), 200_000e6, 1e6);
    }

    /// What `shares` own with the position counted at the venue's oracle,
    /// which here is the one price nobody moved.
    function atOracle(uint256 shares) internal view returns (uint256) {
        Market memory m = venue.marketOf(SPOT);
        int256 venueCash = venue.cashOf(address(station));
        uint256 btc = spotSize() * m.oracle / 1e18;
        uint256 total = vault.cash() + uint256(venueCash) + btc + usd.balanceOf(address(station));
        return total * shares / vault.totalSupply();
    }

    function test_a_mark_and_bid_above_the_oracle_for_one_block_pay_a_leaver_nothing_extra() public {
        (uint256 aliceShares, uint256 bobShares) = halfAtTheVenue();
        uint256 bobBefore = atOracle(bobShares);
        uint256 aliceFair = atOracle(aliceShares);

        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);

        // In the valuation's block the venue's mark, bid and ask sit 20%
        // above its oracle, which has not moved.
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE * 120 / 100, PRICE, PRICE * 120 / 100, PRICE * 121 / 100);
        assertTrue(station.outsideBand(), "a mark 20% off the oracle is not outside the band");
        value(vault);
        // That valuation prices nothing: she isn't paid at the pumped mark.
        assertTrue(latest().status == ValuationStatus.Void, "a valuation at a pumped mark was priced");
        assertEq(usd.balanceOf(alice), 0);

        // The book is back where it was the next block, and the next
        // valuation pays her.
        setPrice(SPOT, PRICE, 0);
        value(vault);
        uint256 paid = usd.balanceOf(alice);
        assertGt(paid, 0, "she was not paid once the market was back");

        emit log_named_uint("alice's worth at the oracle", aliceFair);
        emit log_named_uint("alice was paid from cash", paid);
        emit log_named_uint("bob's worth at the oracle before", bobBefore);
        emit log_named_uint("bob's worth at the oracle after", atOracle(bobShares));

        assertLe(paid, aliceFair, "a leaver was paid from cash above the pool's worth at the oracle");
        assertGe(atOracle(bobShares) + 1e6, bobBefore, "the stayer paid for a mark nobody sold at");
    }

    function test_a_payday_at_a_pumped_mark_mints_no_fee_on_it() public {
        (, uint256 bobShares) = halfAtTheVenue();
        uint256 bobBefore = atOracle(bobShares);
        assertEq(vault.balanceOf(author), 0);

        vm.warp(nextMondayAfter(vm.getBlockTimestamp()));
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE * 120 / 100, PRICE, PRICE * 120 / 100, PRICE * 121 / 100);
        value(vault);
        setPrice(SPOT, PRICE, 0);
        value(vault);

        uint256 feeAtOracle = atOracle(vault.balanceOf(author) + vault.balanceOf(platform));
        emit log_named_uint("fee shares' worth at the oracle", feeAtOracle);
        emit log_named_uint("bob's worth at the oracle before payday", bobBefore);
        emit log_named_uint("bob's worth at the oracle after payday", atOracle(bobShares));
        // The only real profit is the kept transaction fees (0.1% of 200k);
        // 15% of that is $30. The pump is 12k, so the fee on it is $1,800.
        // Plus a week of holding fee to the platform (about $38 on 200k).
        assertLe(feeAtOracle, 75e6, "a fee was minted on a one-block mark");
        assertGe(atOracle(bobShares) + 20e6, bobBefore, "the stayer paid a fee on a gain never realised");
    }

    function test_a_wide_valuation_prices_no_deposit() public {
        (, uint256 bobShares) = halfAtTheVenue();
        uint256 bobBefore = atOracle(bobShares);

        // In the valuation's block the mark and the bid sit 20% below an
        // unchanged oracle. The band rule sees it...
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE * 80 / 100, PRICE, PRICE * 80 / 100, PRICE * 81 / 100);
        assertTrue(station.outsideBand(), "the depressed bid is not wide");
        // ...and the valuation prices nothing, carol's deposit included: it
        // waits for the next one, at the market as it really is.
        bytes memory statement = statementBy(carolKey, address(vault), carol, vault.restrictedText());
        give(carol, 100_000e6);
        vm.startPrank(carol);
        usd.approve(address(vault), 100_000e6);
        vault.requestDeposit(100_000e6, carol, statement);
        vm.stopPrank();
        value(vault);
        assertTrue(latest().status == ValuationStatus.Void, "a wide valuation was priced");
        assertEq(vault.balanceOf(carol), 0, "a deposit was priced at a depressed mark");
        setPrice(SPOT, PRICE, 0);
        value(vault);
        uint256 carolShares = vault.balanceOf(carol);
        assertGt(carolShares, 0);

        emit log_named_uint("bob's worth at the oracle before", bobBefore);
        emit log_named_uint("bob's worth at the oracle after carol joined", atOracle(bobShares));
        emit log_named_uint("carol's worth at the oracle, having paid 100k", atOracle(carolShares));

        assertGe(atOracle(bobShares) + 1e6, bobBefore, "a deposit at a wide valuation diluted the stayer");
        uint256 paid = redeemAs(vault, carol, carolShares);
        emit log_named_uint("carol's round trip returned", paid);
        assertLe(paid, 100_000e6, "a round trip around a depressed mark returned more than went in");
    }
}
