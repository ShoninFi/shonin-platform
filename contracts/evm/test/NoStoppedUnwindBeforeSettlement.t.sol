// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A stopped station can't be unwound between a valuation's pricing and its
/// settlement: what the sale brings home can't pay a withdrawal priced at the
/// earlier snapshot, leaving the holders who stay with the leaver's part of a
/// fall.
contract NoStoppedUnwindBeforeSettlementTest is ClaimBase {
    function _halt() internal {
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        assertTrue(station.stopped());
    }

    function test_a_leaver_is_not_paid_the_snapshot_price_from_a_sale_made_after_it() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        // 190k at the station: 3 BTC at 60k and 10k of venue cash; 10k of
        // cash stays at the vault (the 5% idle floor).
        deploySpot(190_000e6, 190_000e6, 3e18);
        _halt();

        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);

        // A valuation is recorded in its slot and left unsettled, as anyone
        // may do. The market is at 60k, inside the band.
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();

        // The market falls 20% before the slot is settled.
        setPrice(SPOT, PRICE * 80 / 100, 0);

        // A stranger can't bring anything home until the slot has settled:
        // that sale must not pay withdrawals priced before it.
        vm.prank(carol);
        vm.expectRevert(Station.NotAllowed.selector);
        station.unwindStopped(_only("local"));

        // The slot settles: alice's withdrawal, which the cash can't cover,
        // becomes a claim, paid by its own close at the market as it is.
        vault.settle(type(uint256).max);
        vm.prank(carol);
        station.unwindStopped(_only("local"));
        assertEq(spotSize(), 0, "the position is still open");
        uint256 bobWorth = vault.cash() * bobShares / vault.totalSupply();
        assertGe(bobWorth, 100_000e6 - 18_000e6 - 500e6, "bob bore more than his share of the fall");
        aliceShares;
    }

    /// The control: with the unwind made before the valuation (the order the
    /// keeper uses), alice is priced on the sale and bears her share of it.
    function test_control_an_unwind_before_the_valuation_prices_the_leaver_on_the_sale() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 3e18);
        _halt();
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);

        setPrice(SPOT, PRICE * 80 / 100, 0);
        vm.prank(carol);
        station.unwindStopped(_only("local"));

        uint256 before = usd.balanceOf(alice);
        value(vault);
        uint256 alicePaid = usd.balanceOf(alice) - before;
        uint256 poolAfter = vault.cash() + alicePaid;
        uint256 aliceFair = poolAfter * aliceShares / (vault.totalSupply() + aliceShares);
        assertLe(alicePaid, aliceFair + 1, "even the honest order overpays the leaver");
        assertGt(alicePaid, aliceFair - 500e6, "the honest order pays the leaver far less than her part");
    }

    function _only(bytes32 name) internal pure returns (bytes32[] memory names) {
        names = new bytes32[](1);
        names[0] = name;
    }
}
