// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ValuationStatus, VaultState} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";

/// What a Lighter account is worth is known only from the newest post, and a
/// post too old prices nothing: a valuation at it is void. A stopped station
/// whose newest post is stale may still close everything against it (nobody
/// may be left to post), but closing against an old index is not valuing at
/// an old equity: a halted vault never pays withdrawals from its cash at what
/// the account was worth hours or weeks ago, a figure every holder can
/// compare with the market before asking. Once the band is full, everything
/// has been asked for and given time to land, what is still at the exchange
/// stops counting, so the vault closes and pays its holders what came home.
contract StoppedStationsStalePostPricesNothingTest is LighterVault {
    function _stoppedAndUnposted() internal returns (uint256 aliceShares) {
        aliceShares = _fundAndTrade();
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        // The executor is gone: nobody posts again. Hours later the market
        // is a fifth lower, and the account's 40 ETH long has lost $21,600.
        skip(3 hours);
        exchange.setMark(ETH, ETH_PX * 80 / 100);
    }

    function _lastStatus() internal view returns (ValuationStatus) {
        return vault.valuationAt(vault.valuationCount() - 1).status;
    }

    function test_a_stopped_stations_stale_post_voids_the_valuation_while_its_unwind_still_closes() public {
        _stoppedAndUnposted();
        value(vault);
        assertTrue(_lastStatus() == ValuationStatus.Void, "a post three hours old prices nothing");
        // The unwind still places its closing orders against the last index.
        station.unwind(1e18);
        assertEq(exchange.orderCount(), 2, "both sides of the one market, whole, inside the band");
    }

    function test_a_leaver_is_not_paid_from_cash_at_what_the_account_was_worth_hours_ago() public {
        uint256 aliceShares = _stoppedAndUnposted();
        uint16[] memory markets = new uint16[](1);
        markets[0] = ETH;
        // What the vault is really worth: its cash and the account now.
        uint256 worth = vault.cash() + uint256(exchange.equity(_master(), markets));
        uint256 supply = vault.totalSupply();

        // Alice, who can read the market, takes a fifth of her shares out.
        uint256 paid = redeemAs(vault, alice, aliceShares / 5);
        uint256 fair = worth * (aliceShares / 5) / supply;
        assertLe(paid, fair, "paid no more than her part of what the vault is worth");
    }

    /// The executor is gone for good. The keeper's unwinds close the
    /// position once the widening band reaches the market, and bring the
    /// account's dollars home; valuations stay void until the band is full
    /// and a delivery window has passed since the last thing was asked for.
    /// Then the account counts for nothing, a valuation finds the station
    /// empty, and the vault closes with everything that came home.
    function test_a_halted_vault_whose_executor_is_gone_closes_and_pays_its_holders_what_came_home() public {
        uint256 aliceShares = _stoppedAndUnposted();
        uint256 cashAtStop = vault.cash();
        uint256 cameHome;
        for (uint256 week = 0; week < 9; week++) {
            skip(7 days);
            uint256 before = vault.cash();
            station.unwind(1e18);
            exchange.process();
            _collect();
            station.unwind(1e18);
            cameHome += vault.cash() - before;
            value(vault);
            if (week < 6) assertTrue(_lastStatus() == ValuationStatus.Void, "the stale post prices nothing");
        }
        assertGt(cameHome, 128_000e6, "the account's dollars after the close came home");
        assertEq(exchange.size(_master(), ETH), 0, "closed once the band reached the market");
        assertGe(station.stoppedBand(), 10_000, "the band is full");

        // A delivery window after the last unwind asked for anything, the
        // account stops counting and the station reads empty.
        skip(lighter.DELIVERY_WINDOW());
        value(vault);
        assertTrue(_lastStatus() == ValuationStatus.Priced, "priced again");
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);
        // All of it, but the dead shares' part, which goes to the tank.
        assertApproxEqAbs(vault.cash(), cashAtStop + cameHome, 0.1e6, "all of it");

        // The holders take what came home, in proportion.
        vm.prank(alice);
        uint256 toAlice = vault.redeemClosed(aliceShares, alice);
        uint256 bobShares = vault.balanceOf(bob);
        vm.prank(bob);
        uint256 toBob = vault.redeemClosed(bobShares, bob);
        assertApproxEqRel(toAlice, toBob, 0.01e18, "the same for the same stake");
        assertApproxEqRel(toAlice + toBob, cashAtStop + cameHome, 0.002e18, "everything that came home");
    }
}
