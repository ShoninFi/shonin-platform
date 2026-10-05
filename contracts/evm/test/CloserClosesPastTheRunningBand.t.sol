// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Closer} from "../src/Closer.sol";
import {VaultState} from "../src/interfaces/IVault.sol";
import {TwoVenueVault} from "./TwoVenueVault.sol";

/// A stopped station's full unwind sells inside the stopped band, which
/// widens with time, and the Closer values the vault only after that unwind.
/// So once the stopped band has widened past the market, the Closer closes the
/// vault in one transaction even while the running band reads the station as
/// outside it: whether to send the Closer is judged on the stopped unwind, not
/// on `outsideBand()`.
contract CloserClosesPastTheRunningBandTest is TwoVenueVault {
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
    }

    function atSlot() internal {
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
    }

    function test_the_closer_closes_a_vault_the_running_band_reads_as_outside() public {
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 90 / 100, PRICE);
        vm.prank(operator);
        venue2.setPrices(SPOT, PRICE, PRICE, PRICE * 90 / 100, PRICE);
        skip(4 * station.WIDEN_AFTER());
        atSlot();

        // The running band, read before any unwind, has the station outside it.
        assertTrue(station.outsideBand(), "the market is inside the running band");

        closer.closeHalted(address(vault));
        assertTrue(vault.state() == VaultState.Closed, "the Closer did not close the vault");
    }
}
