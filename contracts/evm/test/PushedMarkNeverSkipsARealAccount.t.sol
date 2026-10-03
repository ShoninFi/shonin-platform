// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState, IVault} from "../src/interfaces/IVault.sol";
import {TwoVenueVault} from "./TwoVenueVault.sol";

/// The band is asked about every account before any is judged dust, so a
/// mark pushed outside the band for a block blocks the close instead of
/// making a real account read as nothing and leaving it out.
contract PushedMarkNeverSkipsARealAccountTest is TwoVenueVault {
    uint256 internal constant PUSHED = 53_333e6;

    function setUp() public override {
        super.setUp();
        vm.prank(operator);
        venue2.listMarket(PERP, true, PRICE);
    }

    function deploy() internal {
        vm.startPrank(executor);
        station.pull(vault.cash());
        station.approveKey("local", executor);
        station.approveKey("local2", executor);
        station.deposit("local", 150_000e6);
        station.deposit("local2", 20_000e6);
        vm.stopPrank();
        buySpot(venue, 2.5e18);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 3e18, limitPrice: PRICE, reduceOnly: false});
        vm.prank(executor);
        venue2.place(address(station), orders);
        assertEq(vault.cash(), 0);
        (uint256 m2,) = driver2.valueOf(address(station));
        assertEq(m2, 20_000e6, "venue 2 holds 20k of equity at its own price");
    }

    function push() internal {
        vm.prank(operator);
        venue2.setPrices(PERP, PUSHED, PRICE, PUSHED, PUSHED);
        (uint256 m2,) = driver2.valueOf(address(station));
        assertEq(m2, 0, "at the pushed mark the account reads as nothing");
        assertTrue(station.outsideBand(), "and the station is outside the band");
    }

    function unpush() internal {
        vm.prank(operator);
        venue2.setPrices(PERP, PRICE, PRICE, PRICE, PRICE);
    }

    function test_a_claim_is_not_paid_short_through_an_account_whose_mark_is_pushed_for_a_block() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 150_000e6);
        deploy();
        value(vault);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        uint256 claimId = vault.claimCount();
        assertEq(claimId, 1, "no claim was booked");

        // Bob's worth at the venues' own prices, before.
        (, uint256 exitBefore) = station.value();
        uint256 bobBefore = (vault.cash() + exitBefore) * bobShares / vault.totalSupply();

        push();
        vm.prank(bob);
        vault.settleClaim(claimId);
        unpush();

        ClaimState state = vault.claimAt(claimId).state;
        uint256 paid = usd.balanceOf(alice);
        (, uint256 exitAfter) = station.value();
        uint256 bobAfter = (vault.cash() + exitAfter) * bobShares / vault.totalSupply();
        emit log_named_uint("alice's share at the venues' own prices (usd 6dp)", 100_000e6);
        emit log_named_uint("alice paid (usd 6dp)", paid);
        emit log_named_uint("bob's worth before (usd 6dp)", bobBefore);
        emit log_named_uint("bob's worth after (usd 6dp)", bobAfter);

        // Either the band rule refused the close, or the leaver got their
        // share; never a claim paid short with its shares burned.
        assertTrue(
            state == ClaimState.Waiting || paid >= 99_000e6,
            "the leaver was paid without their share of an account whose mark was pushed for one block"
        );
        assertLe(bobAfter, bobBefore + 100e6, "the stayers kept the leaver's share of the pushed account");
    }

    function test_a_serve_is_not_paid_short_through_an_account_whose_mark_is_pushed_for_a_block() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 150_000e6);
        deploy();
        value(vault);
        // Nothing prices alice's withdrawal for a day (the market sits
        // outside the band), so she takes the fallback: units for her share
        // of the station, cash share zero.
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 5 / 6, PRICE);
        value(vault);
        skip(1 days + 2 * SLOT);
        vm.prank(alice);
        vault.takeProRata(id);
        (uint256 held,) = vault.unitsOf(alice);
        assertApproxEqRel(held, 0.4e18, 0.02e18, "alice holds units for 40% of the station");
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE, PRICE);

        push();
        vm.prank(bob);
        (bool ok,) = address(vault).call(abi.encodeCall(IVault.serveProRata, (alice)));
        unpush();
        (uint256 heldAfter, uint256 claimable) = vault.unitsOf(alice);
        emit log_named_string("serve went through", ok ? "yes" : "no");
        emit log_named_uint("alice's units after (wei of 1e18)", heldAfter);
        emit log_named_uint("alice's claimable (usd 6dp)", claimable);
        emit log_named_uint("alice's share at the venues' own prices (usd 6dp)", 100_000e6);

        assertTrue(
            !ok || heldAfter == held || claimable >= 99_000e6,
            "units were served short through an account whose mark was pushed for one block"
        );
    }
}
