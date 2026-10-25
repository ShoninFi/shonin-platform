// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {HyperliquidVault} from "./HyperliquidVault.sol";

/// A perp the driver doesn't list can't be valued at its exit side, so
/// inside the band it blocks every close and voids every valuation. A full
/// unwind, which every holder shares, is never held by it: it closes the
/// listed markets at any price and brings their dollars home, and the
/// unlisted position, which nothing here can close, is left where it is.
contract AFullUnwindIsNeverHeldByAPerpTheDriverDoesNotListTest is HyperliquidVault {
    uint32 internal constant OTHER = 9;

    function _heldBesideAnUnlistedPerp() internal {
        _fundAndTrade();
        _stockGas();
        // $50 of a perp off the driver's list.
        core.listPerp(OTHER, 2, 5000);
        core.trade(address(station), OTHER, 1000, 5000);
    }

    function test_inside_the_band_an_unlisted_perp_blocks_the_close() public {
        _heldBesideAnUnlistedPerp();
        (, bool blocked) = hl.close(address(station), 0.5e18, 100);
        assertTrue(blocked, "a partial close went ahead beside an unlisted perp");
        (, blocked) = hl.close(address(station), 1e18, 9999);
        assertTrue(blocked, "a full close inside the band went ahead beside an unlisted perp");
        assertTrue(station.outsideBand(), "a valuation would price beside an unlisted perp");
    }

    function test_at_the_full_band_the_listed_markets_are_closed_beside_an_unlisted_perp() public {
        _heldBesideAnUnlistedPerp();
        (, bool blocked) = hl.close(address(station), 1e18, 10_000);
        assertFalse(blocked, "a full-band close was blocked");
    }

    function test_a_stopped_unwind_brings_the_listed_positions_home_beside_an_unlisted_perp() public {
        _heldBesideAnUnlistedPerp();
        uint256 cashBefore = vault.cash();
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
        assertEq(core.szi(address(station), ETH), 0, "the listed position was never closed");
        assertEq(core.szi(address(station), OTHER), 1000, "the unlisted position is left as it was");
        assertGt(vault.cash(), cashBefore + 180_000e6, "the listed position's dollars came home");
    }
}
