// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A slot's deposits are priced on the pool its exits left: an exit paid at
/// the exit side leaves its gap to the mark in the pool, and a deposit in the
/// same slot pays for that gap rather than sharing it, so settling it never
/// lowers a stayer's price and a round trip across the spread returns no
/// more than went in.
contract DepositsPricedOnThePoolTheExitsLeftTest is ClaimBase {
    uint256 internal constant CAROL_IN = 1_300_000e6;

    /// Net worth per share at the mark side, after the profit fee owed now,
    /// on the live state: what a valuation taken now would price a share at.
    function netPerShare() internal view returns (uint256) {
        (uint256 markValue,) = station.value();
        uint256 total = vault.cash() + markValue + 1;
        uint256 supply = vault.totalSupply() + 1e12;
        uint256 basis = ceilDiv(vault.mark() * supply, 1e30);
        uint256 fee = total > basis ? (total - basis) * 1500 / 10_000 : 0;
        return (total - fee) * 1e18 / supply;
    }

    /// 900k at the venue, long 120 BTC-PERP (7.2M notional, 8x).
    function leveredBook() internal {
        vm.startPrank(executor);
        station.pull(900_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 900_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 120e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
    }

    /// Mark and oracle at PRICE, bid `bps` under them, ask `bps` over.
    function spread(uint256 bps) internal {
        vm.prank(operator);
        venue.setPrices(PERP, PRICE, PRICE, PRICE - PRICE * bps / 10_000, PRICE + PRICE * bps / 10_000);
    }

    function setUpSlot(bool withCarol) internal returns (uint256 bobShares) {
        depositAs(vault, alice, 1_000_000e6);
        bobShares = depositAs(vault, bob, 300_000e6);
        leveredBook();
        value(vault);

        // A wide moment, inside the band: the bid 0.95% under the mark.
        spread(95);
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        if (withCarol) {
            bytes memory statement = statementBy(carolKey, address(vault), carol, vault.restrictedText());
            give(carol, CAROL_IN);
            vm.startPrank(carol);
            usd.approve(address(vault), CAROL_IN);
            vault.requestDeposit(CAROL_IN, carol, statement);
            vm.stopPrank();
        }
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
    }

    /// Settling the deposit, after the exit it shares a slot with, leaves
    /// the stayer's price where the exit left it.
    function test_settling_a_same_slot_deposit_keeps_the_stayers_price() public {
        setUpSlot(true);
        // The slot's exit alone (bob's redemption is first in the queue).
        vault.settle(1);
        assertGt(usd.balanceOf(bob), 0, "bob's exit wasn't settled first");
        uint256 afterExit = netPerShare();
        // Then the deposit.
        vault.settle(type(uint256).max);
        assertGt(vault.balanceOf(carol), 0, "carol's deposit wasn't settled");
        uint256 afterDeposit = netPerShare();
        emit log_named_uint("alice's net price after bob's exit  ", afterExit);
        emit log_named_uint("alice's net price after carol joined", afterDeposit);
        // A price per share in the asset's six decimals: allow a unit.
        assertGe(afterDeposit + 1, afterExit, "carol's deposit lowered the stayers' price");
    }

    /// A depositor in the same slot who leaves once the spread closes gets
    /// back no more than they put in.
    function test_a_round_trip_across_a_same_slot_exit_returns_no_more_than_went_in() public {
        uint256 snap = vm.snapshotState();
        setUpSlot(false);
        vault.settle(type(uint256).max);
        spread(0);
        value(vault);
        uint256 aliceWithout = vault.balanceOf(alice) * netPerShare() / 1e18;
        vm.revertToState(snap);

        setUpSlot(true);
        vault.settle(type(uint256).max);
        spread(0);
        uint256 back = redeemAs(vault, carol, vault.balanceOf(carol));
        uint256 aliceWith = vault.balanceOf(alice) * netPerShare() / 1e18;

        emit log_named_uint("carol put in  ", CAROL_IN);
        emit log_named_uint("carol got back", back);
        emit log_named_uint("alice without carol", aliceWithout);
        emit log_named_uint("alice with carol   ", aliceWith);
        assertLe(back, CAROL_IN, "a two-slot round trip returned more than went in");
    }
}
