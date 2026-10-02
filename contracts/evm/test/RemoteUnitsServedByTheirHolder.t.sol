// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, Request} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// On a vault that trades on other chains, a holder's units are a claim on
/// money this chain can't see; only the holder may accept a serve of them.
contract RemoteUnitsServedByTheirHolderTest is TwoChainVault {
    function test_units_on_money_the_last_valuation_never_saw_are_served_only_by_their_holder() public {
        // Both in; the last priced valuation sees everything as cash at home.
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        depositTwo(bob, 100_000e6);
        assertEq(latest().grossExit - 1 - latest().cash, 0, "the last valuation counted some station value");

        // The executor deploys 190k to the other chain, then goes silent:
        // no valuation is priced from here on.
        vm.startPrank(executor);
        station.pull(190_000e6);
        station.bridge("local-bridge", OTHER, 190_000e6, "");
        vm.stopPrank();

        // A day without a price: alice takes the fallback. She gets half of
        // the 10k of cash and units for half of what the stations hold --
        // about 95k on the other chain.
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        Request memory r = vault.requestAt(id);
        vm.warp(r.earliest + 24 hours + 1);
        vm.prank(alice);
        vault.takeProRata(id);
        (uint256 held,) = vault.unitsOf(alice);
        assertApproxEqRel(held, 0.5e18, 0.01e18, "alice did not get half the stations in units");
        assertApproxEqRel(usd.balanceOf(alice), 5000e6, 0.02e18);

        // Bob, who owns the other half, may not serve her units: nothing on
        // this chain can say what the other chain holds.
        vm.prank(bob);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.serveProRata(alice);
        (uint256 heldAfter,) = vault.unitsOf(alice);
        assertEq(heldAfter, held, "alice's units changed");
    }
}
