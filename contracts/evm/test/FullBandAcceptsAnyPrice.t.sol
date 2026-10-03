// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {StationBase} from "./Station.t.sol";

/// Once a stopped station's band has widened all the way, a close is made at
/// any price: a venue whose mark stayed far above its own price, or whose ask
/// for a short did, can't keep the money there for good.
contract FullBandAcceptsAnyPriceTest is StationBase {
    function test_a_mark_stuck_above_twice_the_venues_price_is_unwound_at_the_full_band() public {
        trading();
        // The venue's mark froze at the old price; its own price fell to a
        // third, and the book quotes at the price. The position is a long
        // spot and a short perp, both held here.
        vm.startPrank(operator);
        venue.setPrices(SPOT, PRICE, PRICE / 3, PRICE / 3, PRICE / 3);
        venue.setPrices(PERP, PRICE, PRICE / 3, PRICE / 3, PRICE / 3);
        vm.stopPrank();
        vault.halt();
        skip(vault.haltGrace());
        station.stop();

        skip(7 days * 20);
        assertEq(station.stoppedBand(), 10_000, "the band did not reach any price");
        (, bool blocked) = station.unwind(1e18);
        assertFalse(blocked, "any price is accepted, yet the close is refused");
        assertEq(venue.positionOf(address(station), SPOT).size, 0, "the position is still open");
    }

    function test_a_shorts_ask_above_twice_the_venues_price_is_unwound_at_the_full_band() public {
        trading();
        // An empty book: the only ask for the short to buy back sits far
        // above the venue's price.
        vm.prank(operator);
        venue.setPrices(PERP, PRICE, PRICE, PRICE, PRICE * 3);
        vault.halt();
        skip(vault.haltGrace());
        station.stop();

        skip(7 days * 20);
        assertEq(station.stoppedBand(), 10_000);
        (, bool blocked) = station.unwind(1e18);
        assertFalse(blocked, "any price is accepted, yet the close is refused");
        assertEq(venue.positionOf(address(station), PERP).size, 0, "the short is still open");
    }
}
