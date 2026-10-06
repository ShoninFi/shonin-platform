// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Call} from "../../src/interfaces/IDriver.sol";
import {LighterVault} from "./LighterVault.sol";
import {MockZkLighter} from "./MockZkLighter.sol";

/// A stopped station's keys are gone, so nobody may be able to post again.
/// Its full unwind must still close and bring the money home: against the
/// last post's index, inside the band as it has widened since the stop, and
/// asking the exchange for no withdrawal larger than the account can hold,
/// whether the exchange refuses or trims one that is.
contract StoppedLighterStationClosesTest is LighterVault {
    function _stop() internal {
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
    }

    function test_a_stopped_station_with_a_stale_post_closes_inside_the_band_without_a_new_post() public {
        _fundAndTrade();
        _stop();
        skip(2 hours);
        uint16 band = station.stoppedBand();
        assertLt(band, 10_000, "the band has not reached any price");
        (Call[] memory calls, bool blocked) = lighter.close(address(station), 1e18, band);
        assertFalse(blocked, "no fresh post is needed once stopped");
        assertEq(calls.length, 2, "both sides of the one market, whole");
        // Neither order may fill worse than the band from the last index.
        this.runAs(calls);
        (,,, uint32 sellAt, bool isAsk) = exchange.orders(0);
        assertTrue(isAsk);
        assertEq(sellAt, ETH_PX * (10_000 - band) / 10_000, "a long sells no lower than the band");
        (,,, uint32 buyAt,) = exchange.orders(1);
        assertEq(buyAt, ETH_PX * (10_000 + band) / 10_000, "a short buys no higher");

        // A partial close still needs a fresh post.
        (, blocked) = lighter.close(address(station), 0.5e18, band);
        assertTrue(blocked);
    }

    function runAs(Call[] calldata calls) external {
        for (uint256 i = 0; i < calls.length; i++) {
            vm.prank(address(station));
            (bool ok,) = calls[i].target.call(calls[i].data);
            require(ok, "call");
        }
    }

    function test_a_stopped_stations_sweep_never_asks_for_more_than_the_account_can_hold() public {
        _fundAndTrade();
        _stop();
        uint256 cashBefore = vault.cash();
        // The exchange refuses a withdrawal larger than the account holds.
        exchange.setRefuseOversized(true);
        station.unwind(1e18);
        uint256 asked = exchange.withdrawalCount();
        assertGt(asked, 1, "asked in pieces");
        for (uint256 i = 0; i < asked; i++) {
            (, uint64 amount) = exchange.withdrawals(i);
            assertLe(amount, 2 * 150_000e6, "never more than twice what the account could hold");
        }
        exchange.process();
        assertLt(uint256(exchange.cash(_master())), 1e6, "the account is emptied to under a dollar");
        _collect();
        station.unwind(1e18);
        assertApproxEqAbs(vault.cash(), cashBefore + 150_000e6, 1e6, "everything came home but a dollar");
    }
}
