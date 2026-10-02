// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState, IVault} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";
import {Vm} from "forge-std/Vm.sol";

/// A close whose proceeds go to repay the venue account's margin debt, so
/// the account is left worth more than the stayers' share, can't bring the
/// leaver's worth home: it is undone and the claim, or the serve, waits. The
/// leaver never pays the stayers' share of the account's debt.
contract MarginDebtIsNeverTheLeaversToPayTest is ClaimBase {
    bytes32 private constant UNWOUND = keccak256("Unwound(uint256,uint256,bool)");

    /// 190k at the venue behind `size` of perp bought at 60k with no spread.
    /// With 3.18 BTC the notional is 190,800: the account's cash is -800
    /// against 190,000 of equity, inside the venue's 10% margin.
    function longPerp(uint256 size) internal {
        vm.startPrank(executor);
        station.pull(190_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 190_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: size, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        assertEq(usd.balanceOf(address(station)), 0, "nothing idle at the station");
        assertFalse(station.outsideBand(), "the market is inside the band");
    }

    /// The station's `Unwound(fraction, sentHome, blocked)` from the last call.
    function unwound() internal returns (uint256 fraction, uint256 sentHome) {
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter == address(station) && logs[i].topics[0] == UNWOUND) {
                (fraction, sentHome,) = abi.decode(logs[i].data, (uint256, uint256, bool));
                return (fraction, sentHome);
            }
        }
        revert("no Unwound event");
    }

    function test_a_claim_on_a_long_with_negative_venue_cash_waits_and_sells_nothing() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        longPerp(3.18e18);
        assertEq(venue.cashOf(address(station)), -800e6, "the account's cash is -800");
        uint256 id = claimFor(bob, bobShares);
        int256 sizeBefore = venue.positionOf(address(station), PERP).size;
        uint256 alicePerShare = perShare();

        vm.prank(carol);
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Waiting, "the claim was paid short");
        assertEq(venue.positionOf(address(station), PERP).size, sizeBefore, "something was sold");
        assertEq(perShare(), alicePerShare, "the stayer's worth moved");
    }

    function test_a_claim_on_a_long_with_no_venue_debt_is_paid_in_full() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        // 3.1666666 BTC at 60k is 189,999.996: cash stays at +0.004.
        longPerp(3.1666666e18);
        assertGe(venue.cashOf(address(station)), 0);
        (, uint256 exitBefore) = driver.valueOf(address(station));

        uint256 id = claimFor(bob, bobShares);
        uint256 bobWorth = worth(bobShares);

        vm.recordLogs();
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid);
        (uint256 fraction, uint256 sentHome) = unwound();
        assertEq(sentHome, exitBefore * fraction / WAD);
        assertGe(usd.balanceOf(bob), bobWorth * 9980 / 10_000);
    }

    function test_a_serve_on_a_long_with_negative_venue_cash_waits() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        longPerp(3.18e18);
        vm.prank(alice);
        uint256 request = vault.requestRedeem(aliceShares, alice);
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRata(request);
        (uint256 held,) = vault.unitsOf(alice);
        assertGt(held, 0);

        vm.prank(bob);
        vm.expectRevert(IVault.NotYet.selector);
        vault.serveProRata(alice);
        (uint256 after_,) = vault.unitsOf(alice);
        assertEq(after_, held, "the units were cleared");
    }
}
