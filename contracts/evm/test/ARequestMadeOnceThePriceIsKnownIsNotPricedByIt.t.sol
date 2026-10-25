// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, Request, Valuation} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Anyone may record a single-chain valuation once its price is knowable;
/// a request made in the same block is for a later slot: that valuation
/// doesn't price it, a second one in the slot is refused, and the fallback
/// isn't open to it for a day.
contract ARequestMadeOnceThePriceIsKnownIsNotPricedByItTest is ClaimBase {
    address internal stranger = makeAddr("stranger");

    function test_a_request_in_the_valuations_block_waits_for_a_later_slot() public {
        depositAs(vault, alice, 1000e6);
        // Move the market so the next valuation is a new price, then go to
        // a slot boundary: the price is readable now.
        setPrice(SPOT, PRICE * 11 / 10, 0);
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        // Anyone may record a single-chain valuation.
        vm.prank(stranger);
        vault.recordValuation();
        uint256 count = vault.valuationCount();
        Valuation memory v = vault.valuationAt(count - 1);
        // A request in the same block is for a later slot.
        vm.prank(alice);
        uint256 id = vault.requestRedeem(1e18, alice);
        Request memory r = vault.requestAt(id);
        assertGt(r.earliest, v.time, "request is for a later slot than the valuation just recorded");
        // A second record in the same slot is refused, not overwritten.
        vm.prank(stranger);
        vm.expectRevert(IVault.AlreadyValued.selector);
        vault.recordValuation();
        // Settling now pays nothing for it.
        vault.settle(type(uint256).max);
        assertFalse(vault.requestAt(id).done);
        // Nor can the fallback be taken for a day.
        vm.prank(alice);
        vm.expectRevert(IVault.NotYet.selector);
        vault.takeProRata(id);
    }
}
