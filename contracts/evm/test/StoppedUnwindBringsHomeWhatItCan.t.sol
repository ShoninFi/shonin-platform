// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Closer} from "../src/Closer.sol";
import {VaultState} from "../src/interfaces/IVault.sol";
import {TwoVenueVault} from "./TwoVenueVault.sol";

/// A stopped station's full unwind brings home whatever it can, whether or
/// not the running band reads the station as outside it. The running band's
/// `outsideBand()` is no guide to whether a stopped unwind would sell: the
/// stopped band widens with time, and one wide market leaves the rest of the
/// station free to come home. So whoever empties a stopped station asks the
/// unwind itself what it would bring home.
contract StoppedUnwindBringsHomeWhatItCanTest is TwoVenueVault {
    Closer internal closer;

    function setUp() public override {
        super.setUp();
        closer = new Closer();
        depositAs(vault, alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(100_000e6);
        station.approveKey("local", executor);
        station.approveKey("local2", executor);
        station.deposit("local", 40_000e6);
        station.deposit("local2", 40_000e6);
        vm.stopPrank();
        buySpot(venue, 0.5e18);
        buySpot(venue2, 0.5e18);
        // 20,000 idle dollars stay at the station.
        assertEq(usd.balanceOf(address(station)), 20_000e6);
    }

    function halt() internal {
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
    }

    function atSlot() internal {
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
    }

    /// Both markets 10% under their own price, for good. Four weeks after
    /// the stop the band is 16%: the full unwind sells everything, while the
    /// running band still reads the station as outside it.
    function test_a_stopped_unwind_sells_once_its_band_has_widened_past_the_market() public {
        halt();
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 90 / 100, PRICE);
        vm.prank(operator);
        venue2.setPrices(SPOT, PRICE, PRICE, PRICE * 90 / 100, PRICE);
        skip(4 * station.WIDEN_AFTER());
        assertEq(station.stoppedBand(), 1600);

        assertTrue(station.outsideBand(), "the running band's view");
        uint256 cashBefore = vault.cash();
        (uint256 home, bool blocked) = station.unwind(1e18);
        assertFalse(blocked, "the stopped band did not reach the market");
        assertGt(home, 20_000e6, "less than the idle dollars came home");
        assertEq(vault.cash(), cashBefore + home);
    }

    /// One market stays 10% wide, the other is fine: the Closer reverts as a
    /// whole, so nothing comes home through it, while the full unwind sells
    /// the fine venue and sends it and the idle dollars home, and says the
    /// wide one was left with `blocked`.
    function test_one_wide_market_leaves_the_rest_free_to_come_home_through_the_unwind() public {
        halt();
        vm.prank(operator);
        venue2.setPrices(SPOT, PRICE, PRICE, PRICE * 90 / 100, PRICE);
        assertTrue(station.outsideBand(), "the running band's view");

        atSlot();
        vm.expectRevert();
        closer.closeHalted(address(vault));
        assertFalse(vault.state() == VaultState.Closed);
        assertEq(usd.balanceOf(address(station)), 20_000e6, "the Closer brought something home");

        uint256 cashBefore = vault.cash();
        (uint256 home, bool blocked) = station.unwind(1e18);
        assertTrue(blocked, "the wide market was not reported as left");
        assertGt(home, 20_000e6, "the fine venue did not come home with the idle dollars");
        assertEq(vault.cash(), cashBefore + home);
        assertEq(usd.balanceOf(address(station)), 0, "idle dollars stayed at the station");
        assertEq(uint256(venue.positionOf(address(station), SPOT).size), 0, "the fine venue is still open");
        assertGt(
            uint256(venue2.positionOf(address(station), SPOT).size),
            0,
            "the wide venue was sold outside the band"
        );
    }
}
