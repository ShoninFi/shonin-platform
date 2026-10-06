// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";

/// The exchange refuses an order smaller than its market's smallest (in
/// size, or in dollars). A claim's close would order the leaver's part of
/// each position; a part under that minimum can't be ordered, so it is not,
/// and the proof asks nothing of it (as of dust): the stage is proved on the
/// rest, and the leaver is paid their part of the account's worth, the
/// unsold piece at its mark, never left with a stage that proves nothing.
contract APieceUnderTheSmallestOrderIsNeitherOrderedNorAskedOfTest is LighterVault {
    function test_a_claims_part_of_a_position_under_the_smallest_order_is_not_ordered_and_the_close_is_paid()
        public
    {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.approveKey(LIGHTER, executor);
        station.pull(150_000e6);
        station.deposit(LIGHTER, 150_000e6);
        station.approveKey(LIGHTER, executor);
        vm.stopPrank();
        // 0.003 ETH: half of it is under the exchange's 0.0020 ETH minimum.
        exchange.trade(_master(), ETH, 30);
        skip(lighter.DELIVERY_WINDOW());
        _post();
        _aliceClaims(aliceShares);

        vault.settleClaim(1);
        assertEq(uint256(vault.claimAt(1).state), uint256(ClaimState.Closing), "staged");
        assertEq(exchange.orderCount(), 0, "nothing under the smallest order was ordered");

        _settleStaged();
        assertEq(station.stage().stagedAt, 0, "the stage was proved and delivered");
        assertEq(uint256(vault.claimAt(1).state), uint256(ClaimState.Paid), "the claim is paid");
        assertGe(usd.balanceOf(alice), 99_000e6, "her half, less the fee");
        assertEq(exchange.size(_master(), ETH), 30, "the position stays whole");
    }
}
