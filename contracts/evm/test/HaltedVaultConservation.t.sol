// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Tank, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A probe of the newest paths together: a market outside the band voids
/// every valuation, a holder takes the fallback (cash share and units), the
/// vault is killed, the station is stopped, the band widens over the weeks
/// until the stopped unwind sells everything at a 10% discount, the vault
/// closes, and everyone collects. Expected to pass: every dollar is either
/// paid to someone, in the tank, or dust; the leaver through the fallback
/// and the holder who stayed bear the same fall.
contract HaltedVaultConservationTest is ClaimBase {
    function _only(bytes32 name) internal pure returns (bytes32[] memory names) {
        names = new bytes32[](1);
        names[0] = name;
    }

    function _nextSlot() internal {
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
    }

    function test_fallback_units_then_a_widened_stopped_unwind_then_close_leaves_nothing_stranded() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        // 150k to the station: 100k at the venue, 60k of it one BTC; 50k idle.
        deploySpot(150_000e6, 100_000e6, 1e18);
        value(vault);
        uint256 poolBefore = usd.balanceOf(address(vault)) + usd.balanceOf(address(station))
            + uint256(venue.cashOf(address(station))) + 60_000e6 - vault.tank().balance;

        // The bid sits 10% under the venue's own price, for good: void.
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 90 / 100, PRICE);
        vm.prank(alice);
        uint256 leaving = vault.requestRedeem(aliceShares, alice);
        _nextSlot();
        vault.recordValuation();
        assertFalse(vault.unsettled(), "a void valuation priced something");

        // A day on, alice takes the fallback: her part of the cash now, and
        // units for her part of the station.
        skip(1 days + SLOT);
        vm.prank(alice);
        vault.takeProRata(leaving);
        uint256 aliceCash = usd.balanceOf(alice);
        (uint256 live,) = vault.units();
        assertGt(live, 0.4e18, "alice holds less than her part in units");
        assertLt(live, 0.6e18, "alice holds more than her part in units");

        // Killed; stopped after the grace; the band widens week by week until
        // it reaches the 10% discount (16% in the fifth week).
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        skip(7 days * 4);
        vm.prank(carol);
        (uint256 sentHome, bool blocked) = station.unwindStopped(_only("local"));
        assertFalse(blocked, "the widened band did not reach the market");
        assertEq(spotSize(), 0, "the position is still open");
        assertEq(usd.balanceOf(address(station)), 0, "idle dollars stayed at the station");
        assertGt(sentHome, 140_000e6, "the sale brought less home than it should");

        // Nothing at the venue, so the next valuation finds the station
        // empty and the vault closes.
        _nextSlot();
        vault.recordValuation();
        vault.settle(type(uint256).max);
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);

        // Everyone collects.
        vm.prank(alice);
        vault.collectUnits();
        vm.prank(bob);
        vault.redeemClosed(bobShares, bob);
        uint256 authorShares = vault.balanceOf(author);
        if (authorShares != 0) {
            vm.prank(author);
            vault.redeemClosed(authorShares, author);
        }
        uint256 platformShares = vault.balanceOf(platform);
        if (platformShares != 0) {
            vm.prank(platform);
            vault.redeemClosed(platformShares, platform);
        }
        vm.prank(author);
        vault.withdrawTankUnsigned(type(uint256).max);

        // Nothing is stranded: the vault holds only what is still owed.
        (, uint256 reserve) = vault.units();
        // A unit (a millionth of a dollar) can stay in the reserve from
        // rounding `perUnit` down; noted.
        assertLe(reserve, 2, "unit money left unclaimed");
        assertLe(vault.cash(), 2, "cash left with nobody to claim it");
        assertLe(usd.balanceOf(address(vault)), vault.escrow() + vault.heldFor(alice) + 2, "money stranded");

        // Every dollar went somewhere: alice, bob, the fee recipients, the
        // author's tank (dead shares' part), or the venue (the 10% discount).
        uint256 paidOut = usd.balanceOf(alice) + usd.balanceOf(bob) + usd.balanceOf(author)
            + usd.balanceOf(platform) - MINIMUM_TANK;
        // The sale cost 10% of one BTC: 6,000. Both halves bore it.
        assertGe(paidOut + 6000e6 + 10e6, poolBefore, "money vanished");
        assertLe(paidOut, poolBefore - 6000e6 + 10e6, "money appeared");

        // Alice (fallback, then units) and bob (stayed to the close) each put
        // in 100k and each bore half the fall; neither paid for the other.
        uint256 aliceTotal = usd.balanceOf(alice);
        uint256 bobTotal = usd.balanceOf(bob);
        assertGt(aliceTotal, 96_000e6, "alice paid for more than her share");
        assertGt(bobTotal, 96_000e6, "bob paid for more than his share");
        assertLt(aliceTotal, 97_500e6, "alice was paid more than her share");
        assertLt(bobTotal, 97_500e6, "bob was paid more than his share");
        aliceCash;
    }
}
