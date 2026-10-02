// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// On a vault that trades on other chains, nobody can serve a leaver's
/// claim by closing at the home station alone: the claim waits, whole, until
/// the cash at home covers it.
contract NoHomeOnlyCloseTest is TwoChainVault {
    function test_no_one_can_serve_a_leavers_claim_at_the_home_station_alone() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        depositTwo(bob, 100_000e6);

        vm.startPrank(executor);
        station.pull(190_000e6);
        station.bridge("local-bridge", OTHER, 150_000e6, "");
        vm.stopPrank();
        valueWith(150_000e6, 150_000e6);
        assertApproxEqAbs(vault.totalValue(), 200_000e6, 1e6);

        // Alice asks to leave; the cash can't pay her, so it is a claim.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        valueWith(150_000e6, 150_000e6);
        assertTrue(vault.claimAt(1).state == ClaimState.Pending);

        // Bob, who stays, serves her claim before any unwinder brings her
        // half of the other chain home.
        // On a vault with stations elsewhere nothing is closed at home alone:
        // the claim waits, unpaid and whole, until the cash covers it.
        vm.prank(bob);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "a home-only close served the claim");
        assertEq(usd.balanceOf(alice), 0, "she was paid part of her share");
        assertEq(vault.claimAt(1).shares, aliceShares);
    }
}
