// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {StationPost, ValuationStatus} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// Money still on its way past the transit timeout has no price: a valuation
/// that would count it prices nothing, so nobody buys shares while it counts
/// at zero and sells once it lands.
contract LateTransitPricesNothingTest is TwoChainVault {
    function test_a_deposit_is_not_priced_while_a_send_is_late() public {
        depositTwo(alice, 200_000e6);

        // The strategy sends half the pool to the other chain. The bridge is
        // late: it hasn't landed an hour later.
        vm.startPrank(executor);
        station.pull(100_000e6);
        station.bridge("local-bridge", OTHER, 100_000e6, "");
        vm.stopPrank();
        skip(1 hours + 1 minutes);

        // Bob sees the late send on-chain and asks to deposit 100k.
        requestDepositAs(vault, bob, 100_000e6);

        // The executor's honest post: nothing at the other station yet, and
        // home send 0 still on its way. It counts at zero.
        uint64 time = open();
        StationPost memory p = postOf(0, 0);
        p.inFlightFromHome = new uint64[](1);
        p.inFlightFromHome[0] = 0;
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);
        assertTrue(latest().status == ValuationStatus.Void, "a valuation counting a late send was priced");
        assertEq(vault.balanceOf(bob), 0, "bob bought shares while the send counted at zero");

        // The send lands; the other station now holds the 100k, and bob's
        // deposit is priced on all of it.
        valueWith(100_000e6, 100_000e6);
        uint256 bobShares = vault.balanceOf(bob);
        assertGt(bobShares, 0);

        // Bob leaves at once, and takes nothing of alice's.
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        valueWith(100_000e6, 100_000e6);
        assertLe(usd.balanceOf(bob), 100_000e6, "a deposit and a withdrawal returned more than was put in");
    }
}
