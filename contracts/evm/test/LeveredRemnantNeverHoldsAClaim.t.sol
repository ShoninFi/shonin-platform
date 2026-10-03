// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState, IVault, VaultState} from "../src/interfaces/IVault.sol";
import {TwoVenueVault} from "./TwoVenueVault.sol";

/// A close is refused for paying nobody only when the leaver's part of an
/// account under water at the bid is worth more than the vault's dust, so a
/// remnant of a levered position at one venue never holds a large claim or
/// lets anyone freeze the vault.
contract LeveredRemnantNeverHoldsAClaimTest is TwoVenueVault {
    /// 16,666,666,667 of size at $60,000 is a notional of 1000 units.
    uint256 internal constant REMNANT = 16_666_666_667;

    function arrange() internal returns (uint256 claimId) {
        vm.prank(operator);
        venue2.listMarket(PERP, true, PRICE);

        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);

        // Everything to the station: 100k at venue 1 as 1 BTC spot and 40k
        // of venue cash; at venue 2, 109 units of margin behind a 1000-unit
        // perp (cash -891); the rest idle at the station.
        vm.startPrank(executor);
        station.pull(vault.cash());
        station.approveKey("local", executor);
        station.approveKey("local2", executor);
        station.deposit("local", 100_000e6);
        vm.stopPrank();
        buySpot(venue, 1e18);
        vm.startPrank(executor);
        station.deposit("local2", 109);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: REMNANT, limitPrice: PRICE, reduceOnly: false});
        venue2.place(address(station), orders);
        vm.stopPrank();
        assertEq(vault.cash(), 0);
        assertEq(venue2.cashOf(address(station)), -891);

        // BTC falls 10% at venue 2 with the bid at the band's edge: the
        // remnant is worth 9 units at the mark and nothing at the bid.
        vm.prank(operator);
        venue2.setPrices(PERP, 54_000e6, 54_000e6, 53_460e6, 54_000e6);
        (uint256 m2, uint256 x2) = driver2.valueOf(address(station));
        assertEq(m2, 9, "the remnant is not 9 units at the mark");
        assertEq(x2, 0, "the remnant is not under water at the bid");
        assertFalse(station.outsideBand(), "every market is inside the band");
        value(vault);

        // Alice (half the vault) asks to leave; no cash, so a claim.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        claimId = vault.claimCount();
        assertEq(claimId, 1, "no claim was booked");
        assertTrue(vault.claimAt(claimId).state == ClaimState.Pending);
    }

    function test_a_hundred_thousand_dollar_claim_is_not_held_by_four_units_of_underwater_remnant() public {
        uint256 claimId = arrange();
        vm.prank(bob);
        vault.settleClaim(claimId);
        // Her part of the remnant is 4 units at the mark, under the vault's
        // dust of 9 for two drivers; venue 1 holds 100k in good order.
        assertTrue(
            vault.claimAt(claimId).state == ClaimState.Paid,
            "a sub-cent underwater remnant at one venue held a 100k claim on a healthy vault"
        );
    }

    function test_nobody_can_freeze_the_vault_with_it() public {
        uint256 claimId = arrange();
        vm.prank(bob);
        vault.settleClaim(claimId);
        skip(7 days);
        vm.prank(bob);
        (bool ok,) = address(vault).call(abi.encodeCall(IVault.freeze, (claimId)));
        assertFalse(ok, "the vault was frozen by a sub-cent remnant");
        assertTrue(vault.state() == VaultState.Open, "the vault was frozen");
    }
}
