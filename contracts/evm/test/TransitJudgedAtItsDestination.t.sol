// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {
    IVault,
    StationPost,
    Transit,
    Valuation,
    ValuationStatus,
    VaultState
} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// A send home is on its way until it has landed by the valuation's open,
/// in block and transaction order, when the home station was read; and a
/// post reads its station at or after the open. Whenever a send home is
/// made or lands, it is counted once.
contract TransitJudgedAtItsDestinationTest is TwoChainVault {
    /// Alice's 100k: 90k on the other chain, which sends 50k home.
    function ninetyAway() internal {
        depositTwo(alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(90_000e6);
        station.bridge("local-bridge", OTHER, 90_000e6, "");
        vm.stopPrank();
        valueWith(90_000e6, 90_000e6);
    }

    function test_a_send_home_landing_after_the_open_is_in_transit() public {
        ninetyAway();
        requestDepositAs(vault, bob, 100_000e6);
        uint64 time = open();
        assertEq(vault.openedAt(vault.valuationCount() - 1), time);

        // Lands at home at T + 4, after the home station was read; the
        // other chain's moment is T + 6.
        vm.warp(time + 4);
        vm.prank(operator);
        bridge.deliver(OTHER, 1, address(station), 50_000e6);

        // It had not landed by the open, so the post names it on its way.
        vm.warp(time + 10);
        StationPost memory p = postOf(40_000e6, 40_000e6);
        p.asOf = time + SLOT + 6;
        p.transit = new Transit[](1);
        p.transit[0] = Transit({amount: 50_000e6, sentAt: time - 1 minutes});
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);

        Valuation memory v = latest();
        assertTrue(v.status == ValuationStatus.Priced);
        assertApproxEqRel(v.gross, 100_000e6, 0.001e18, "the send home was not counted once");
        uint256 supply = vault.totalSupply();
        assertApproxEqRel(vault.balanceOf(bob) * 1e18 / supply, 0.5e18, 0.01e18, "bob's price was off");
    }

    function test_a_send_home_landing_before_the_open_is_at_home() public {
        ninetyAway();
        uint256 t0 = vm.getBlockTimestamp() + 2 * SLOT - 1;
        uint64 time = uint64(t0 - (t0 % SLOT)) + 120 * SLOT;

        // Relayed at T + 1; the open is recorded at T + 3, so the home
        // station is read with it there.
        vm.warp(time + 1);
        vm.prank(operator);
        bridge.deliver(OTHER, 1, address(station), 50_000e6);
        vm.warp(time + 3);
        vm.prank(executor);
        vault.recordValuation();

        // It had landed by the open: no longer on its way. The post reads
        // the other station at or after the open.
        StationPost memory p = postOf(40_000e6, 40_000e6);
        p.asOf = time + SLOT + 3;
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);

        Valuation memory v = latest();
        assertTrue(v.status == ValuationStatus.Priced);
        assertApproxEqRel(v.gross, 100_000e6, 0.001e18, "the send home was not counted once");
    }

    function test_a_halted_vault_does_not_close_with_a_send_home_still_landing() public {
        ninetyAway();
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE + 1);

        uint64 time = open();
        vm.warp(time + 4);
        vm.prank(operator);
        bridge.deliver(OTHER, 1, address(station), 90_000e6);
        vm.warp(time + 10);
        StationPost memory p = postOf(0, 0);
        p.asOf = time + SLOT + 6;
        p.transit = new Transit[](1);
        p.transit[0] = Transit({amount: 90_000e6, sentAt: time - 1 minutes});
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);

        vm.expectRevert(IVault.NotYet.selector);
        vault.close();
        assertTrue(vault.state() != VaultState.Closed);
    }

    function nextTime() internal view returns (uint64) {
        uint256 t = vm.getBlockTimestamp() + 2 * SLOT - 1;
        return uint64(t - (t % SLOT));
    }

    function test_a_landing_in_the_opens_block_after_the_open_is_in_transit() public {
        ninetyAway();
        requestDepositAs(vault, bob, 100_000e6);
        uint64 time = nextTime();
        vm.warp(time);
        vm.prank(executor);
        vault.recordValuation();
        assertEq(vault.openedBlock(vault.valuationCount() - 1), block.number);
        // Same block, after the open transaction.
        vm.prank(operator);
        bridge.deliver(OTHER, 1, address(station), 50_000e6);

        StationPost memory p = postOf(40_000e6, 40_000e6);
        p.transit = new Transit[](1);
        p.transit[0] = Transit({amount: 50_000e6, sentAt: time - 1 minutes});
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);
        assertApproxEqRel(latest().gross, 100_000e6, 0.001e18, "the send home was not counted once");
    }

    function test_a_landing_in_the_opens_block_before_the_open_is_at_home() public {
        ninetyAway();
        requestDepositAs(vault, bob, 100_000e6);
        uint64 time = nextTime();
        vm.warp(time);
        // Same block, before the open transaction.
        vm.prank(operator);
        bridge.deliver(OTHER, 1, address(station), 50_000e6);
        vm.prank(executor);
        vault.recordValuation();

        post(time, postOf(40_000e6, 40_000e6), EXECUTOR_KEY);
        closeAfterDeadline(time);
        assertApproxEqRel(latest().gross, 100_000e6, 0.001e18, "the send home was not counted once");
    }

    function test_a_post_reads_its_station_at_or_after_the_open() public {
        ninetyAway();
        requestDepositAs(vault, bob, 100_000e6);
        uint64 time = nextTime();
        // The other station sends 50k home at T + 1 and it lands at once;
        // the open is at T + 2.
        vm.warp(time + 1);
        vm.prank(operator);
        bridge.deliver(OTHER, 1, address(station), 50_000e6);
        vm.warp(time + 2);
        vm.prank(executor);
        vault.recordValuation();

        // A post as of T, before the open, is refused.
        StationPost memory early = postOf(90_000e6, 90_000e6);
        early.asOf = time;
        bytes memory sig = signed(EXECUTOR_KEY, vault.postDigest(time, OTHER, early));
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.postStation(time, OTHER, early, sig);

        // As of the open, the 50k had left: 40k there, 50k at home.
        post(time, postOf(40_000e6, 40_000e6), EXECUTOR_KEY);
        closeAfterDeadline(time);
        assertApproxEqRel(latest().gross, 100_000e6, 0.001e18, "the send home was not counted once");
        uint256 supply = vault.totalSupply();
        assertApproxEqRel(vault.balanceOf(bob) * 1e18 / supply, 0.5e18, 0.01e18, "bob's price was off");
    }

    /// Clocks and block times differ between chains, so a post reads its
    /// station a slot or more after the open: a send home that the open's
    /// block saw land was made before the post's moment, and is not named.
    function test_a_post_reads_a_slot_after_the_open_so_a_send_landed_before_it_counts_once() public {
        ninetyAway();
        requestDepositAs(vault, bob, 100_000e6);
        uint64 time = nextTime();
        vm.warp(time);
        // The other station's send lands at home just ahead of the open.
        vm.prank(operator);
        bridge.deliver(OTHER, 1, address(station), 50_000e6);
        vm.prank(executor);
        vault.recordValuation();

        // A post stamped within the slot after the open is refused.
        StationPost memory early = postOf(90_000e6, 90_000e6);
        early.asOf = time + SLOT - 1;
        bytes memory sig = signed(EXECUTOR_KEY, vault.postDigest(time, OTHER, early));
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.postStation(time, OTHER, early, sig);

        // A slot on, the send had left and landed: 40k there, nothing named.
        post(time, postOf(40_000e6, 40_000e6), EXECUTOR_KEY);
        closeAfterDeadline(time);
        assertApproxEqRel(latest().gross, 100_000e6, 0.001e18, "the send home was not counted once");
        uint256 supply = vault.totalSupply();
        assertApproxEqRel(vault.balanceOf(bob) * 1e18 / supply, 0.5e18, 0.01e18, "bob's price was off");
    }
}
