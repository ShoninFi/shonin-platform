// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Valuation} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// Disagreeing posts never price a deposit above the highest deposit-side
/// figure any post gave: each post's mark and premium are kept together.
contract DepositNeverAboveTheHighestPostTest is TwoChainVault {
    uint256 internal constant PRICE_SCALE = 1e30;
    uint256 internal constant BPS = 10_000;

    function feeOn(uint256 gross, uint256 supply) internal view returns (uint256) {
        uint256 basis = (vault.mark() * supply + PRICE_SCALE - 1) / PRICE_SCALE;
        if (gross <= basis) return 0;
        return (gross - basis) * 1500 / BPS;
    }

    function sharesAt(uint256 assets, uint256 gross, uint256 supply) internal view returns (uint256) {
        uint256 txFee = (assets * 10 + BPS - 1) / BPS;
        return (assets - txFee) * supply / (gross - feeOn(gross, supply));
    }

    function test_a_deposit_is_never_priced_above_the_highest_post() public {
        depositTwo(alice, 100_000e6);
        requestDepositAs(vault, bob, 100_000e6);

        uint64 time = open();
        // The executor says 10k at mark, 12k at exit; the unwinder says 11k
        // at mark, 9k at exit.
        post(time, postOf(10_000e6, 12_000e6), EXECUTOR_KEY);
        post(time, postOf(11_000e6, 9000e6), UNWINDER_KEY);
        vm.warp(time + DEADLINE + 1);
        vault.closeValuation(time);
        Valuation memory v = latest();
        assertEq(v.gross, v.cash + 10_000e6 + 1, "the lowest mark");
        assertEq(v.grossExit, v.cash + 9000e6 + 1, "the lowest exit");

        // What bob's deposit would buy at the highest deposit-side figure
        // any post carried (12k), and at the combined figure (13k).
        uint256 atHighestPost = sharesAt(100_000e6, v.cash + 12_000e6 + 1, v.supply);
        uint256 atCombined = sharesAt(100_000e6, v.cash + 13_000e6 + 1, v.supply);

        vault.settle(type(uint256).max);
        uint256 bobShares = vault.balanceOf(bob);
        emit log_named_uint("shares at the highest single post (12k)", atHighestPost);
        emit log_named_uint("shares at mark 11k + premium 2k (13k)", atCombined);
        emit log_named_uint("shares bob got", bobShares);

        assertGe(bobShares, atHighestPost, "the deposit was priced above every figure either post reported");
    }
}
