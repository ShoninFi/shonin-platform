// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, Request} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A deposit nothing has priced for a day goes back only when a valuation
/// now would price nothing either. While one could, its owner can't record
/// it, see the price, and then choose between buying and taking the money
/// back: the deposit is priced like any other.
contract DepositFallbackOnlyWhenNothingCanPriceTest is ClaimBase {
    function test_a_deposit_a_valuation_could_price_is_not_sent_back() public {
        depositAs(vault, alice, 100_000e6);
        uint256 id = requestDepositAs(vault, bob, 10_000e6);
        // Nobody values the vault for a day.
        Request memory r = vault.requestAt(id);
        vm.warp(r.earliest + 1 days + 1);

        vm.prank(bob);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.takeProRata(id);

        value(vault);
        assertTrue(vault.requestAt(id).done, "the deposit was not priced");
        assertGt(vault.balanceOf(bob), 0, "bob has no shares");
    }

    function test_a_deposit_goes_back_when_the_market_is_outside_the_band() public {
        depositAs(vault, alice, 100_000e6);
        deploySpot(50_000e6, 50_000e6, 0.1e18);
        uint256 id = requestDepositAs(vault, bob, 10_000e6);
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 90 / 100, PRICE);
        Request memory r = vault.requestAt(id);
        vm.warp(r.earliest + 1 days + 1);

        vm.prank(bob);
        vault.takeProRata(id);
        assertEq(usd.balanceOf(bob), 10_000e6, "bob's deposit did not come back");
    }
}
