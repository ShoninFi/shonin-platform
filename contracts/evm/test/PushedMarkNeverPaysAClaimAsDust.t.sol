// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState, IVault, ValuationStatus} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A mark pushed outside the band for a block makes neither a claim nor a
/// holder's units look like dust: the close is blocked and they wait.
contract PushedMarkNeverPaysAClaimAsDustTest is ClaimBase {
    /// 190k of margin behind 28 BTC of perp at 60k: 11.3% margin.
    function deployLevered() internal {
        vm.startPrank(executor);
        station.pull(190_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 190_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 28e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        assertEq(usd.balanceOf(address(station)), 0, "nothing idle at the station");
    }

    /// The equity of 28 BTC bought with 190k of margin reads as zero at
    /// this mark: 28 * 53,214 = 1,489,992 against 1,490,000 owed.
    function pushTheMark() internal {
        vm.prank(operator);
        venue.setPrices(PERP, 53_214e6, PRICE, 53_214e6, 53_214e6);
        (uint256 atMark,) = station.value();
        assertEq(atMark, 0, "the pushed mark makes the whole station read as nothing");
        assertTrue(station.outsideBand(), "the station is outside the band at this price");
    }

    function test_a_claim_served_through_a_pushed_mark_is_paid_its_cash_share_as_dust() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        // The last priced valuation: everything is cash, the station empty.
        value(vault);
        uint256 lastPriced = vault.valuationCount();

        // Then the strategy deploys 190k, and alice asks to leave.
        deployLevered();
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        // The market sits outside the band at her slot, and stays there:
        // the valuation is void and nothing prices her withdrawal.
        vm.prank(operator);
        venue.setPrices(PERP, PRICE, PRICE, PRICE * 5 / 6, PRICE);
        value(vault);
        assertTrue(vault.valuationAt(vault.valuationCount() - 1).status == ValuationStatus.Void, "not void");
        assertEq(vault.valuationCount(), lastPriced + 1);

        // A day on, she books a claim instead of taking the fallback.
        skip(1 days + 2 * SLOT);
        vm.prank(alice);
        uint256 claimId = vault.claimInstead(id);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Pending);

        // In the block the claim is served, the mark is pushed 11% below
        // the oracle and the account reads as nothing.
        pushTheMark();
        vm.prank(bob);
        vault.settleClaim(claimId);

        emit log_named_uint("alice paid (usd 6dp)", usd.balanceOf(alice));
        emit log_named_uint("alice's share at the oracle (usd 6dp)", 100_000e6);

        // The band rule: a close through a market outside the band is
        // refused and the claim waits. Instead it is paid as dust.
        assertTrue(
            vault.claimAt(claimId).state == ClaimState.Waiting,
            "a claim was judged dust and paid its cash share through a pushed mark"
        );
        assertEq(usd.balanceOf(alice), 0, "the leaver was paid a twentieth of their share and burned");
    }

    function test_units_served_through_a_pushed_mark_are_cleared_for_nothing() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        value(vault);
        deployLevered();

        // Alice's withdrawal is never priced (the market is outside the
        // band), so after a day she takes the fallback: half the cash and
        // units for half of what the station holds.
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        vm.prank(operator);
        venue.setPrices(PERP, PRICE, PRICE, PRICE * 5 / 6, PRICE);
        value(vault);
        skip(1 days + 2 * SLOT);
        vm.prank(alice);
        vault.takeProRata(id);
        (uint256 held,) = vault.unitsOf(alice);
        assertApproxEqRel(held, 0.5e18, 0.02e18, "alice holds units for about half the station");

        // Anyone serves her in the block the mark is pushed.
        pushTheMark();
        vm.prank(bob);
        (bool ok, bytes memory reason) = address(vault).call(abi.encodeCall(IVault.serveProRata, (alice)));
        if (!ok) emit log_named_bytes("serve refused with", reason);
        (uint256 heldAfter, uint256 claimable) = vault.unitsOf(alice);
        emit log_named_uint("alice's units after (wei of 1e18)", heldAfter);
        emit log_named_uint("alice's claimable after (usd 6dp)", claimable);

        // A serve through a market outside the band must be refused
        // (`NotYet`, as `ServeJudgedAtTheMark` demands when the station
        // refuses the close); it must not clear units worth 95k at the
        // oracle for nothing.
        assertTrue(
            !ok || heldAfter == held || claimable >= 90_000e6,
            "units worth 95k at the oracle were cleared for nothing through a pushed mark"
        );
    }
}
