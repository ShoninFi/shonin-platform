// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ValuationStatus} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// A listed market the venue stops answering for (its reads fail) neither
/// stops the station being valued nor stops the other markets closing. Its
/// figures are never guessed: inside the band it blocks the close, so a
/// valuation is recorded void and prices nothing, and the station's value
/// is never more than it was with the market readable. A full unwind closes
/// every market it can read and leaves that one where it is.
contract AMarketTheVenueStopsAnsweringForHoldsNothingElseTest is HyperliquidVault {
    /// The account long ETH and holding $100 of HYPE above its reserve.
    function _heldInTwoMarkets() internal {
        _fundAndTrade();
        _stockGas();
        core.creditSpot(address(station), HYPE_TOKEN, 5e8);
        core.setSpot(HYPE_PAIR, 20_000_000, 19_990_000, 20_010_000);
    }

    function _priced() internal view returns (bool) {
        return vault.valuationAt(vault.valuationCount() - 1).status == ValuationStatus.Priced;
    }

    function test_an_unreadable_perp_voids_the_valuation_and_never_raises_the_value() public {
        _heldInTwoMarkets();
        (uint256 markBefore, uint256 exitBefore) = hl.valueOf(address(station));
        core.breakPerp(ETH);
        (uint256 mark, uint256 exit) = hl.valueOf(address(station));
        assertLe(mark, markBefore, "the mark rose when a read failed");
        assertLe(exit, exitBefore, "the exit side rose when a read failed");
        (, bool blocked) = hl.close(address(station), 0.5e18, 100);
        assertTrue(blocked, "a partial close went ahead with a market unread");
        value(vault);
        assertFalse(_priced(), "a valuation priced with a market unread");
    }

    function test_an_unreadable_spot_pair_voids_the_valuation_and_never_raises_the_value() public {
        _heldInTwoMarkets();
        (uint256 markBefore, uint256 exitBefore) = hl.valueOf(address(station));
        core.breakPair(HYPE_PAIR);
        (uint256 mark, uint256 exit) = hl.valueOf(address(station));
        assertLe(mark, markBefore, "the mark rose when a read failed");
        assertLe(exit, exitBefore, "the exit side rose when a read failed");
        value(vault);
        assertFalse(_priced(), "a valuation priced with a market unread");
    }

    function test_an_unreadable_token_balance_voids_the_valuation() public {
        _heldInTwoMarkets();
        core.breakToken(HYPE_TOKEN);
        hl.valueOf(address(station));
        (, bool blocked) = hl.close(address(station), 0.5e18, 100);
        assertTrue(blocked, "a partial close went ahead with a balance unread");
        value(vault);
        assertFalse(_priced(), "a valuation priced with a balance unread");
    }

    function test_a_full_unwind_closes_the_perp_when_the_spot_pair_cannot_be_read() public {
        _heldInTwoMarkets();
        core.breakPair(HYPE_PAIR);
        (, bool blocked) = hl.close(address(station), 1e18, 10_000);
        assertFalse(blocked, "a full-band close was blocked by one unread market");
        _stoppedAndUnwound();
        assertEq(core.szi(address(station), ETH), 0, "the readable perp was never closed");
    }

    function test_a_full_unwind_sells_the_spot_holding_when_the_perp_cannot_be_read() public {
        _heldInTwoMarkets();
        core.breakPerp(ETH);
        uint256 hype = core.spotTotal(address(station), HYPE_TOKEN);
        _stoppedAndUnwound();
        assertLt(
            core.spotTotal(address(station), HYPE_TOKEN), hype, "the readable spot holding was never sold"
        );
    }

    function _stoppedAndUnwound() internal {
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        // The band doubles weekly until it is full; an unwind each week.
        for (uint256 i = 0; i < 10; i++) {
            _whenMovable();
            skip(7 days);
            station.unwind(1e18);
            _nextBlock();
        }
    }
}
