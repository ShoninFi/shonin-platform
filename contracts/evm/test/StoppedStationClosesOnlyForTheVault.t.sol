// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Once stopped, a station closes a fraction only for the vault's own claims
/// and serves, which wait for settlement; the executor can't close part of it
/// between a valuation's pricing and its settlement.
contract StoppedStationClosesOnlyForTheVaultTest is ClaimBase {
    function test_a_stopped_station_refuses_the_executors_fractional_unwind_before_settlement() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(150_000e6, 150_000e6, 2.5e18);
        value(vault);

        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        vm.prank(carol);
        station.stop();
        assertTrue(station.stopped());

        // Bob asks for more than the cash can pay; a valuation prices it at
        // 60k and is not yet settled.
        vm.prank(bob);
        vault.requestRedeem(bobShares * 8 / 10, bob);
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        assertTrue(vault.unsettled(), "nothing waits to settle");
        uint256 cashAtSnapshot = vault.cash();

        // A passer-by's full unwind waits for settlement.
        vm.prank(carol);
        vm.expectRevert(Station.NotAllowed.selector);
        station.unwind(1e18);

        // The market falls 5% before anything settles.
        setPrice(SPOT, 57_000e6, 0);

        // The executor's fractional unwind should wait too.
        vm.prank(executor);
        (bool ok,) = address(station).call(abi.encodeCall(Station.unwind, (0.5e18)));

        // Otherwise: the sale at 57k lands as cash, and bob's withdrawal,
        // priced at 60k, is paid from it instead of becoming a claim.
        if (ok) {
            uint256 broughtHome = vault.cash() - cashAtSnapshot;
            emit log_named_uint("cash at the snapshot", cashAtSnapshot);
            emit log_named_uint("brought home by the sale at 57k", broughtHome);
            uint256 before = usd.balanceOf(bob);
            vault.settle(type(uint256).max);
            emit log_named_uint("bob paid, at the 60k snapshot", usd.balanceOf(bob) - before);
            emit log_named_uint("claims booked", vault.claimCount());
        }
        assertFalse(
            ok, "the executor unwound a stopped station between a valuation's pricing and its settlement"
        );
    }
}
