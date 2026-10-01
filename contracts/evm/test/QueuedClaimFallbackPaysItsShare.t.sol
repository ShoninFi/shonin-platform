// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A queued claim taken through the fallback is paid what its close brought
/// home and its share of the rest of the cash: its fraction, no more and no
/// less.
contract QueuedClaimFallbackPaysItsShareTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function hedge(uint256 amount) internal {
        vm.startPrank(executor);
        station.pull(amount);
        station.approveKey("local", executor);
        station.deposit("local", amount);
        OrderRequest[] memory orders = new OrderRequest[](2);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        orders[1] =
            OrderRequest({market: PERP, isBuy: false, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        setPrice(SPOT, PRICE, 0);
    }

    function test_a_queued_claim_taken_through_the_fallback_gets_its_fraction() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        // 190k at the venue, 10k of cash: alice's withdrawal becomes a claim.
        hedge(190_000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);

        // Her fraction is closed and comes home; the claim waits to be priced.
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);

        // What her shares are worth now: their fraction of the cash and of
        // what the station still holds, at exit prices. No fee is charged on
        // the fallback, so this is the most she may be paid.
        (, uint256 stationExit) = station.value();
        uint256 fair = (vault.cash() + stationExit) * aliceShares / vault.totalSupply();

        // Nothing is priced for a day, and she takes the fallback.
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRataClaim(1);

        assertLe(usd.balanceOf(alice), fair, "the fallback paid the claim more than its fraction");
        assertApproxEqRel(
            usd.balanceOf(alice), fair, 0.001e18, "the fallback paid the claim less than its fraction"
        );
    }
}
