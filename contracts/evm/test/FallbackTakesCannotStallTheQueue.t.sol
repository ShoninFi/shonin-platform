// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, Request} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// Requests taken through the pro-rata fallback can't stall the queue: the
/// cursor passes them a bounded number at a time, in `settle` (every request
/// passed counts toward its `max`) and before a valuation, so no call ever
/// needs more than a bounded amount of gas, however many there are.
contract FallbackTakesCannotStallTheQueueTest is TwoChainVault {
    uint256 internal constant BLOCK_GAS = 30_000_000;
    uint256 internal constant N = 300;
    uint256 internal constant BOUNDED = 3_000_000;

    function brick() internal returns (uint256 firstRequest) {
        depositTwo(alice, 100_000e6);
        depositTwo(bob, 100e6);

        vm.startPrank(bob);
        firstRequest = vault.requestRedeem(1, bob);
        for (uint256 i = 1; i < N; i++) {
            vault.requestRedeem(1, bob);
        }
        vm.stopPrank();

        // The other chain goes quiet for a day: nothing prices the requests.
        Request memory r = vault.requestAt(firstRequest);
        vm.warp(r.earliest + 24 hours + 1);

        vm.startPrank(bob);
        for (uint256 i = 0; i < N; i++) {
            vault.takeProRata(firstRequest + i);
        }
        vm.stopPrank();

        // The keeper's call is its own transaction: every slot it reads is
        // cold, as it would be on chain.
        vm.cool(address(vault));
    }

    function test_settle_passes_them_in_bounded_steps_and_a_valuation_follows() public {
        brick();
        uint256 steps;
        while (true) {
            uint256 before = gasleft();
            vault.settle(64);
            assertLt(before - gasleft(), BOUNDED, "one settle walked too far");
            vm.cool(address(vault));
            steps++;
            if (vault.requestAt(vault.requestCount()).done && steps > N / 64) break;
            if (steps > N) revert("the cursor never passed them");
        }
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        uint256 gasBefore = gasleft();
        vm.prank(executor);
        vault.recordValuation();
        assertLt(gasBefore - gasleft(), BOUNDED, "the valuation walked the queue");
    }

    function test_a_valuation_never_walks_them_all() public {
        brick();
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        uint256 before = gasleft();
        (bool ok,) = address(vault).call{gas: BLOCK_GAS}(abi.encodeCall(IVault.recordValuation, ()));
        ok;
        assertLt(before - gasleft(), BOUNDED, "a valuation walked every request taken through the fallback");
    }
}
