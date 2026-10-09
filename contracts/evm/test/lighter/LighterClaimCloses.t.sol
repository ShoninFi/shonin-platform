// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LighterPosition, LighterReport} from "../../src/drivers/lighter/LighterBoard.sol";
import {LighterVault} from "./LighterVault.sol";

/// A claim's close on Lighter is sized from a post: only a recent one, so it
/// never closes more of a position than the fraction because the strategy
/// traded since; never when the accounts couldn't release its worth after
/// it; and it is proved account by account, so a long in one account and a
/// short in another each answer for their own orders.
contract LighterClaimClosesTest is LighterVault {
    function test_a_claims_close_is_sized_only_from_a_post_a_few_minutes_old_at_most() public {
        _fundAndTrade();
        (, bool blocked) = lighter.close(address(station), 0.5e18, 100);
        assertFalse(blocked, "a post just made");
        skip(lighter.STAGE_POST_AGE() + 1);
        (, blocked) = lighter.close(address(station), 0.5e18, 100);
        assertTrue(blocked, "older: nothing is ordered from it");
        assertFalse(station.outsideBand(), "though it still prices");
        _post();
        (, blocked) = lighter.close(address(station), 0.5e18, 100);
        assertFalse(blocked);
    }

    function test_a_claims_close_the_accounts_could_not_release_is_refused_before_anything_is_sold() public {
        _fundAndTrade();
        // Everything the account holds is margin for its position.
        skip(1);
        LighterReport memory r = _reading();
        r.available[0] = 0;
        vm.prank(executor);
        board.post(address(station), r);
        (, bool blocked) = lighter.close(address(station), 0.5e18, 100);
        assertTrue(blocked, "half the margin freed can't pay half the account");
        // With $60k free beside the margin, it can.
        skip(1);
        r = _reading();
        r.available[0] = 60_000e6;
        vm.prank(executor);
        board.post(address(station), r);
        (, blocked) = lighter.close(address(station), 0.5e18, 100);
        assertFalse(blocked);
    }

    function test_a_long_in_one_account_and_a_short_in_another_each_answer_for_their_own_close() public {
        _fundAndTrade();
        uint48 master = _master();
        uint48 sub = master + 1;
        skip(1);
        LighterReport memory r = _twoAccounts(master, sub, 10_000, -10_000);
        vm.prank(executor);
        board.post(address(station), r);
        int256[] memory before = lighter.snapshot(address(station));
        assertEq(before[0], 0, "netted, the market holds nothing");

        // Only the long's order filled.
        skip(lighter.SETTLE_DELAY());
        r = _twoAccounts(master, sub, 5000, -10_000);
        vm.prank(executor);
        board.post(address(station), r);
        (uint256 part, bool known) = lighter.closedPart(address(station), before, 0.5e18, uint64(r.asOf));
        assertTrue(known);
        assertEq(part, 0, "the short's close did nothing");

        // Both filled.
        skip(1);
        r = _twoAccounts(master, sub, 5000, -5000);
        vm.prank(executor);
        board.post(address(station), r);
        (part,) = lighter.closedPart(address(station), before, 0.5e18, 0);
        assertEq(part, 1e18);
    }

    function _twoAccounts(
        uint48 master,
        uint48 sub,
        int64 longSize,
        int64 shortSize
    ) internal view returns (LighterReport memory r) {
        r = _reading();
        r.accounts = new uint48[](2);
        r.accounts[0] = master;
        r.accounts[1] = sub;
        r.available = new uint256[](2);
        r.available[0] = 100_000e6;
        r.positions = new LighterPosition[](2);
        r.positions[0] = LighterPosition({account: master, market: ETH, size: longSize});
        r.positions[1] = LighterPosition({account: sub, market: ETH, size: shortSize});
    }
}
