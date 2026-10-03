// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, ValuationStatus} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Between a valuation's pricing and its settlement nothing else takes the
/// cash: the home station can't pull it and no unit holder's serve runs,
/// until what the valuation priced is paid. Both go through once it is.
contract NothingLeavesWhileAPricedValuationWaitsTest is ClaimBase {
    function setUp() public override {
        super.setUp();
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 20_000e6);
        deploySpot(60_000e6, 50_000e6, 0.5e18);
        value(vault);
    }

    function nextBoundary() internal view returns (uint256) {
        uint256 t = vm.getBlockTimestamp();
        return t - (t % SLOT) + SLOT;
    }

    /// Bob asks out and a valuation prices his withdrawal, unsettled.
    function priceAWithdrawal() internal {
        uint256 shares = vault.balanceOf(bob) / 2;
        vm.prank(bob);
        vault.requestRedeem(shares, bob);
        vm.warp(nextBoundary());
        vault.recordValuation();
        assertTrue(latest().status == ValuationStatus.Priced, "the valuation was not priced");
        assertTrue(vault.unsettled(), "the valuation has nothing to settle");
    }

    function test_the_station_cannot_pull_cash_a_priced_valuation_is_to_pay_from() public {
        priceAWithdrawal();
        uint256 cash = vault.cash();
        vm.prank(executor);
        vm.expectRevert(IVault.WrongState.selector);
        station.pull(1000e6);
        assertEq(vault.cash(), cash, "cash left while the valuation waited");

        vault.settle(type(uint256).max);
        assertFalse(vault.unsettled());
        vm.prank(executor);
        station.pull(1000e6);
        assertLt(vault.cash(), cash, "the pull was refused once the valuation was settled");
    }

    function test_a_serve_waits_for_a_priced_valuation_to_settle() public {
        // Nothing prices for a day: alice takes the fallback for a tenth of
        // her shares and holds units.
        setPrice(SPOT, PRICE, 500);
        uint256 tenth = vault.balanceOf(alice) / 10;
        vm.prank(alice);
        uint256 id = vault.requestRedeem(tenth, alice);
        vm.warp(vault.requestAt(id).earliest + 1 days);
        vm.prank(alice);
        vault.takeProRata(id);
        setPrice(SPOT, PRICE, 0);
        (uint256 units,) = vault.unitsOf(alice);
        assertGt(units, 0, "alice holds no units");

        priceAWithdrawal();
        uint256 size = spotSize();
        vm.expectRevert(IVault.NotYet.selector);
        vault.serveProRata(alice);
        assertEq(spotSize(), size, "a serve sold while the valuation waited");
        (uint256 left,) = vault.unitsOf(alice);
        assertEq(left, units, "units were cleared while the valuation waited");

        vault.settle(type(uint256).max);
        vault.serveProRata(alice);
        (left,) = vault.unitsOf(alice);
        assertEq(left, 0, "the serve was refused once the valuation was settled");
    }
}
