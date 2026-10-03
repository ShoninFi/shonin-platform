// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Claim, ClaimState, IVault, VaultState} from "../src/interfaces/IVault.sol";
import {TwoVenueVault} from "./TwoVenueVault.sol";

/// A close is refused for paying nobody only when the leaver's part of an
/// account is worth more than dust at the mark, so a sub-cent remnant of a
/// position at one venue never holds a claim, nor lets anyone freeze the
/// vault with one.
contract RemnantPositionNeverHoldsAClaimTest is TwoVenueVault {
    uint256 internal aliceShares;
    uint256 internal claimId;

    function arrange() internal {
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);

        vm.startPrank(executor);
        station.pull(vault.cash());
        station.approveKey("local", executor);
        station.approveKey("local2", executor);
        station.deposit("local", 120_000e6);
        station.deposit("local2", 1000);
        vm.stopPrank();
        buySpot(venue, 2e18);
        // 16_666_666_667 of size at $60,000 is a notional of 1000 units.
        buySpot(venue2, 16_666_666_667);
        spread(venue, 100);
        spread(venue2, 100);
        assertEq(vault.cash(), 0, "the vault keeps no cash");
        (uint256 m2, uint256 x2) = driver2.valueOf(address(station));
        assertEq(m2, 1000, "the remnant is 1000 units at the mark");
        assertEq(x2, 995, "the remnant is 995 units at the bid");
        assertGt(usd.balanceOf(address(station)), 79_000e6, "the station has ~80k idle");
        value(vault);

        // A fraction of 1.003e-3: one unit of the remnant at the mark
        // (1.003), none at the bid (0.998).
        aliceShares = (vault.totalSupply() + 1e12) * 1003 / 1_000_000;
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        claimId = vault.claimCount();
        assertEq(claimId, 1, "no claim was booked");
        assertTrue(vault.claimAt(claimId).state == ClaimState.Pending);
    }

    function test_a_two_hundred_dollar_claim_on_a_liquid_station_is_served() public {
        arrange();
        assertFalse(station.outsideBand(), "every market is inside the band");
        vm.prank(bob);
        vault.settleClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Paid, "a sub-cent remnant held the claim");
    }

    function test_the_remnant_cannot_be_used_to_freeze_the_vault() public {
        arrange();
        vm.prank(bob);
        vault.settleClaim(claimId);
        skip(7 days);
        vm.prank(carol());
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.freeze(claimId);
        assertTrue(vault.state() != VaultState.Frozen, "the vault was frozen");
    }

    function carol() internal returns (address) {
        return makeAddr("carol");
    }
}
