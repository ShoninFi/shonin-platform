// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, StationPost, Transit, Valuation, ValuationStatus} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// Every post reads its station as of one moment, the station chain's first
/// block at or after the valuation's time, and says which: money that moves
/// between chains while the posts come in is counted once, on the chain it
/// left, and a post can't be dated outside its window or name a send made
/// after its moment.
contract PostsReadOneMomentTest is TwoChainVault {
    function test_a_send_home_after_the_posts_moment_is_counted_once() public {
        depositTwo(alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(90_000e6);
        station.bridge("local-bridge", OTHER, 90_000e6, "");
        vm.stopPrank();
        valueWith(90_000e6, 90_000e6);

        requestDepositAs(vault, bob, 100_000e6);
        uint64 time = open();

        // After the post's moment the other station sends 50k home, and it
        // lands at the home station, valued at the open before it did.
        vm.warp(time + 20);
        vm.prank(operator);
        bridge.deliver(OTHER, 1, address(station), 50_000e6);

        // The post, made later, reads the other station as of its moment:
        // all 90k still there.
        vm.warp(time + 30);
        StationPost memory p = postOf(90_000e6, 90_000e6);
        p.asOf = time + SLOT;
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);

        Valuation memory v = latest();
        assertTrue(v.status == ValuationStatus.Priced);
        assertApproxEqRel(v.gross, 100_000e6, 0.001e18, "the send home was not counted once");
        uint256 supply = vault.totalSupply();
        assertApproxEqRel(vault.balanceOf(bob) * 1e18 / supply, 0.5e18, 0.01e18, "bob's price was off");
    }

    function test_a_post_outside_its_window_or_naming_a_later_send_is_refused() public {
        depositTwo(alice, 100_000e6);
        uint64 time = open();
        StationPost memory p = postOf(0, 0);

        // Dated less than a slot after the open, or past its post window.
        p.asOf = time + SLOT - 1;
        bytes memory sig = signed(EXECUTOR_KEY, vault.postDigest(time, OTHER, p));
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.postStation(time, OTHER, p, sig);
        p.asOf = time + DEADLINE + 1;
        sig = signed(EXECUTOR_KEY, vault.postDigest(time, OTHER, p));
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.postStation(time, OTHER, p, sig);

        // Naming a send made after its moment.
        p.asOf = time + SLOT + 5;
        p.transit = new Transit[](1);
        p.transit[0] = Transit({amount: 10_000e6, sentAt: time + SLOT + 6});
        sig = signed(EXECUTOR_KEY, vault.postDigest(time, OTHER, p));
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.postStation(time, OTHER, p, sig);

        // As of its moment, with a send made before it.
        p.transit[0].sentAt = time + SLOT + 5;
        vm.warp(time + SLOT + 10);
        post(time, p, EXECUTOR_KEY);
    }
}
