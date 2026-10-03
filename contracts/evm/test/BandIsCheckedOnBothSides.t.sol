// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ValuationStatus} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A valuation is void when any market's mark, bid or ask sits further from
/// the venue's own price than the band on either side, so a bid pushed far
/// above it for a block can't charge a deposit a premium nobody could sell at.
contract BandIsCheckedOnBothSidesTest is ClaimBase {
    uint256 internal constant BPS = 10_000;

    /// What `shares` own at the venue's mark.
    function atMark(uint256 shares) internal view returns (uint256) {
        (uint256 mark,) = station.value();
        return (vault.cash() + mark) * shares / vault.totalSupply();
    }

    function _pool() internal {
        depositAs(vault, alice, 100_000e6);
        // 90k at the venue as 1.5 BTC at 60k; 10k of cash stays (the floor).
        deploySpot(90_000e6, 90_000e6, 1.5e18);
        value(vault);
        assertApproxEqAbs(vault.totalValue(), 100_000e6, 1e6);
    }

    /// The handbook's rule: a bid 50% above the venue's own price is outside
    /// the band on the far side, so the valuation must be void.
    function test_a_bid_far_above_the_venues_price_voids_the_valuation_on_either_side() public {
        _pool();
        requestDepositAs(vault, bob, 100_000e6);

        // In the valuation's block the bid and ask sit 50% above the venue's
        // own price (the mark and oracle are untouched, so the mark check
        // passes).
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 150 / 100, PRICE * 151 / 100);
        assertTrue(station.outsideBand(), "a bid 50% above the venue's price is not judged outside the band");
        value(vault);
        assertTrue(
            latest().status == ValuationStatus.Void,
            "a valuation with a market 50% off the venue's price on the far side priced a deposit"
        );
    }
}
