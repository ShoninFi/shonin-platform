// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Valuation, ValuationStatus} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// For any three disagreeing posts, deposits pay the highest figure any post
/// carried, withdrawals the lowest exit (never above the lowest mark), and
/// fees are measured on the lowest mark; and a deposit followed by a
/// withdrawal at the same valuation never returns more than was put in.
contract PostsCombineTowardThePoolTest is TwoChainVault {
    function testFuzz_posts_combine_toward_the_pool(uint64[6] memory raw) public {
        depositTwo(alice, 100_000e6);
        uint256[6] memory f;
        for (uint256 i = 0; i < 6; i++) {
            f[i] = uint256(raw[i]) % 200_000e6;
        }
        requestDepositAs(vault, bob, 50_000e6);
        uint64 time = open();
        uint256 lowMark = type(uint256).max;
        uint256 lowExit = type(uint256).max;
        uint256 top;
        for (uint256 i = 0; i < 3; i++) {
            uint256 m = f[2 * i];
            uint256 x = f[2 * i + 1];
            post(time, postOf(m, x), i % 2 == 0 ? EXECUTOR_KEY : UNWINDER_KEY);
            if (m < lowMark) lowMark = m;
            uint256 cx = x > m ? m : x;
            if (cx < lowExit) lowExit = cx;
            uint256 t = x > m ? x : m;
            if (t > top) top = t;
        }
        vm.warp(time + DEADLINE + 1);
        vault.closeValuation(time);
        Valuation memory v = latest();
        assertTrue(v.status == ValuationStatus.Priced);
        assertEq(v.gross, v.cash + lowMark + 1, "fees not on the lowest mark");
        assertEq(v.grossExit, v.cash + lowExit + 1, "withdrawals not at the lowest exit");

        // Bob buys, then sells everything back at this same valuation's price.
        uint256 gTop = v.cash + top + 1;
        uint256 basis = (vault.mark() * v.supply + 1e30 - 1) / 1e30;
        uint256 feeTop = gTop > basis ? (gTop - basis) * 1500 / 10_000 : 0;
        vault.settle(type(uint256).max);
        uint256 shares = vault.balanceOf(bob);
        if (shares == 0) return;
        // At most what the highest post's price buys.
        assertLe(shares, (50_000e6 - 50e6) * v.supply / (gTop - feeTop), "bought below the highest post");
        // A withdrawal of those shares priced at this snapshot.
        uint256 slice = v.feeExit == 0 ? 0 : (shares * v.feeExit + v.grossExit - 1) / v.grossExit;
        uint256 back = (shares - slice) * v.grossExit / v.supply;
        back -= (back * 10 + 9999) / 10_000;
        assertLe(back, 50_000e6, "a round trip returned more than was put in");
    }
}
