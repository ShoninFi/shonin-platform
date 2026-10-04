// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "../src/Vault.sol";
import {Fixture} from "./Fixture.sol";

/// What a collection leaves of the usage cap is kept for the next one, up
/// to a day's cost at the allowances. A strategy spends in bursts, most of
/// all in a halt's grace while it winds down, and gas is allowed per day,
/// not per minute: a bill collected piece by piece is charged the same as
/// one collection of the same final total. But allowance left unused for
/// longer than a day is not saved up: a wrong total can take at most about
/// a day beyond what the time since the last collection allowed.
contract UsageCapCarriesForADayTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault(); // $2 a day of gas allowed
    }

    function test_collecting_piece_by_piece_charges_the_same_as_once() public {
        // Twelve hours of running: $1 of cap, $0.40 used.
        skip(12 hours);
        vm.prank(admin);
        vault.kill();
        uint64 halted = uint64(vm.getBlockTimestamp());

        // In its grace the strategy closes its positions: $0.50 of gas in
        // its second minute, inside the day's $2.
        uint64 firstAsOf = halted + 60;
        uint64 lastAsOf = halted + 120;
        uint256 firstTotal = 0.4e6;
        uint256 lastTotal = 0.9e6;

        uint256 snapshot = vm.snapshotState();

        // Once, with the final total.
        vm.warp(lastAsOf);
        uint256 before = usd.balanceOf(platform);
        vault.keep(lastTotal, lastAsOf, usage(vault, lastTotal, lastAsOf));
        uint256 once = usd.balanceOf(platform) - before;
        assertEq(once, lastTotal, "one collection charges everything used");

        vm.revertToState(snapshot);

        // Each newer total, a minute apart.
        before = usd.balanceOf(platform);
        vm.warp(firstAsOf);
        vault.keep(firstTotal, firstAsOf, usage(vault, firstTotal, firstAsOf));
        vm.warp(lastAsOf);
        vault.keep(lastTotal, lastAsOf, usage(vault, lastTotal, lastAsOf));
        uint256 pieceByPiece = usd.balanceOf(platform) - before;

        assertEq(pieceByPiece, once, "collecting piece by piece charged differently from once");
    }

    function test_the_carried_cap_never_lets_more_than_the_allowances_be_charged() public {
        // A day at $2 a day, $0.50 used: $1.50 of cap is kept.
        skip(1 days);
        uint64 first = uint64(vm.getBlockTimestamp());
        uint256 before = usd.balanceOf(platform);
        vault.keep(0.5e6, first, usage(vault, 0.5e6, first));
        assertApproxEqAbs(vault.tank().capCarried, 1.5e6, 1, "the unused cap was not kept");

        // Another day, and a bill far above the allowances: charged up to
        // two days' worth in all, the rest written off.
        skip(1 days);
        uint64 second = uint64(vm.getBlockTimestamp());
        vault.keep(100e6, second, usage(vault, 100e6, second));
        assertApproxEqAbs(usd.balanceOf(platform) - before, 4e6, 2, "more than two days' allowances");
        assertLe(vault.tank().capCarried, 1, "a cap that was used up was kept");
    }

    function test_months_of_unused_allowance_cannot_be_charged_in_one_collection() public {
        // Ninety days collected daily with nothing used: $180 of allowance
        // went unused, and no more than a day's $2 of it is kept.
        for (uint256 i = 0; i < 90; i++) {
            skip(1 days);
            // The executor's key is blessed again as it runs.
            blessExecutorKey(executor, address(vault), bundle);
            uint64 at = uint64(vm.getBlockTimestamp());
            vault.keep(0, at, usage(vault, 0, at));
        }
        assertApproxEqAbs(vault.tank().capCarried, 2e6, 1, "more than a day's allowance was kept");

        // An hour later, a total far above the allowances: a day's carried
        // cap and the hour's own, no more.
        give(author, 500e6);
        vm.startPrank(author);
        usd.approve(address(vault), 500e6);
        vault.fundTank(500e6);
        vm.stopPrank();
        skip(1 hours);
        blessExecutorKey(executor, address(vault), bundle);
        uint64 late = uint64(vm.getBlockTimestamp());
        uint256 before = usd.balanceOf(platform);
        vault.keep(500e6, late, usage(vault, 500e6, late));
        uint256 hour = uint256(2e6) / 24;
        assertApproxEqAbs(usd.balanceOf(platform) - before, 2e6 + hour, 2, "months of allowance were charged");
    }
}
