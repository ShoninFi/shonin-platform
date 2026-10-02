// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Request} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// A unit holder is served only for money their serve brought home: when a
/// station holds nothing at home but the vault's money sits on another chain,
/// a serve is refused and the holder keeps their units.
contract ServeNeedsMoneyHomeTest is TwoChainVault {
    function test_a_serve_that_brings_nothing_home_burns_no_units() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        depositTwo(bob, 100_000e6);

        // The strategy deploys 150k on the other chain; 50k of cash stays.
        vm.startPrank(executor);
        station.pull(150_000e6);
        station.bridge("local-bridge", OTHER, 150_000e6, "");
        vm.stopPrank();
        valueWith(150_000e6, 150_000e6);

        // The other chain goes quiet for a day: alice leaves through the
        // fallback with half the cash and units for half the stations.
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        Request memory r = vault.requestAt(id);
        vm.warp(r.earliest + 24 hours + 1);
        vm.prank(alice);
        vault.takeProRata(id);
        (uint256 held, uint256 claimableBefore) = vault.unitsOf(alice);
        assertApproxEqRel(held, 0.5e18, 0.01e18, "alice holds units for about half the stations");
        assertApproxEqAbs(usd.balanceOf(alice), 25_000e6, 100e6);

        // A passer-by "serves" her at the empty home station.
        vm.prank(bob);
        (bool ok,) = address(vault).call(abi.encodeWithSignature("serveProRata(address)", alice));

        (uint256 heldAfter, uint256 claimableAfter) = vault.unitsOf(alice);
        uint256 paid = claimableAfter - claimableBefore;
        // Either the serve was refused, or it paid her what her units were
        // worth (about 75k on the other chain). What must not happen is her
        // units going to zero for nothing.
        assertTrue(
            !ok || heldAfter == held || paid + 100e6 >= 75_000e6,
            "a serve at an empty home station burned her units and paid nothing"
        );
    }
}
