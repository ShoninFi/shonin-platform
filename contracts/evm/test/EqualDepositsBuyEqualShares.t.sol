// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Valuation} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Two equal deposits in one slot buy the same shares, with the bid above
/// the mark and a profit fee owed.
contract EqualDepositsBuyEqualSharesTest is ClaimBase {
    function test_two_equal_deposits_in_one_slot_buy_the_same_shares_when_the_bid_is_above_the_mark()
        public
    {
        // One holder; 100k of the 200k at the venue in spot BTC (1.5 BTC at
        // 60k plus 10k of venue cash).
        depositCarol(200_000e6);
        deploySpot(100_000e6, 100_000e6, 1.5e18);
        value(vault);

        // BTC rises to 70k: the vault is above its mark, so a profit fee is
        // owed. Alice and bob each ask to put in 50k in the same slot.
        setPrice(SPOT, 70_000e6, 0);
        requestDepositAs(vault, alice, 50_000e6);
        requestDepositAs(vault, bob, 50_000e6);

        // In the valuation's block the bid sits above the mark: the exit
        // side exceeds the mark side and deposits pay the premium on top.
        vm.prank(operator);
        venue.setPrices(SPOT, 70_000e6, 70_000e6, 75_000e6, 76_000e6);
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        Valuation memory v = latest();
        assertGt(v.fee, 0, "no profit fee owed; the order effect needs one");
        uint256 markBefore = vault.mark();

        vault.settle(type(uint256).max);
        uint256 aliceShares = vault.balanceOf(alice);
        uint256 bobShares = vault.balanceOf(bob);
        emit log_named_uint("mark before settling", markBefore);
        emit log_named_uint("mark after settling", vault.mark());
        emit log_named_uint("alice's shares (settled first)", aliceShares);
        emit log_named_uint("bob's shares (settled second)", bobShares);

        // Same dollars, same valuation: the same shares, up to a wei.
        assertApproxEqAbs(
            aliceShares, bobShares, 1, "the second deposit in the slot was priced differently from the first"
        );
    }
}
