// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimBase} from "./ClaimBase.sol";

/// Whoever settles a valuation chooses how many requests one call settles,
/// so a long queue settles in calls that each fit a small block (HyperEVM's
/// small blocks hold 2,000,000 gas), and a halted vault's close finds
/// nothing left to settle.
contract SettlementGoesInBoundedBatchesTest is ClaimBase {
    uint256 internal constant SMALL_BLOCK = 2_000_000;

    function test_two_hundred_requests_settle_in_calls_that_each_fit_a_small_block() public {
        depositAs(vault, alice, 10_000e6);
        deploySpot(5000e6, 4000e6, 0.05e8);
        uint256 n = 100;
        give(alice, n * MINIMUM_DEPOSIT);
        vm.startPrank(alice);
        usd.approve(address(vault), n * MINIMUM_DEPOSIT);
        for (uint256 i = 0; i < n; i++) {
            vault.requestRedeem(1e15, alice);
            vault.requestDeposit(MINIMUM_DEPOSIT, alice, "");
        }
        vm.stopPrank();
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        uint256 calls;
        while (vault.unsettled()) {
            uint256 before = gasleft();
            vault.settle(50);
            assertLt(before - gasleft(), SMALL_BLOCK, "one batch was larger than a small block");
            calls++;
            require(calls < 10, "settling never ended");
        }
        assertGe(calls, 4, "fewer batches than the queue needs");
        assertEq(vault.nextUnpriced(), 0, "a request was left unsettled");
    }
}
