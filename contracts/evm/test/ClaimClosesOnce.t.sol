// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A claim closes its fraction once. Its fraction left the stations when it
/// was closed, so the valuation that prices it pays what that brought home
/// and its share of the rest of the cash, never a share of what the stations
/// made since: it never bounces back to be closed again, and the stayers
/// keep their gains.
contract ClaimClosesOnceTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    /// One BTC long against one BTC short, with a 1% spread on spot (bid
    /// 59,700, ask 60,300), inside the 1% exit band.
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
        setPrice(SPOT, PRICE, 100);
    }

    function test_a_claim_priced_after_the_station_gained_is_paid_its_own_and_closes_once() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        // 190k at the venue; 10k of cash (the 5% floor).
        hedge(190_000e6);

        // Alice (half the vault) leaves; the cash can't pay her: a claim.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);

        // Served: her half of the positions is closed and ~95k comes home.
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);
        int256 spotAfterFirstClose = venue.positionOf(address(station), SPOT).size;
        assertApproxEqAbs(uint256(spotAfterFirstClose), 0.5e18, 0.001e18, "her half was closed");
        uint256 cashHome = vault.cash();
        assertGt(cashHome, 100_000e6);

        // Before the next slot the station gains 50k (25% of the pool). The
        // gain is the stayers': the next valuation pays her what her close
        // brought home and half the rest of the cash, and closes nothing more.
        give(address(station), 50_000e6);
        uint256 owned = vault.cash() / 2 + 47_500e6;
        value(vault);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "the claim bounced");
        assertLe(usd.balanceOf(alice), owned + 1e6, "the claim was paid part of the stayers' gain");
        assertApproxEqRel(usd.balanceOf(alice), owned, 0.01e18);
        assertEq(venue.positionOf(address(station), SPOT).size, spotAfterFirstClose, "the claim closed more");
    }
}
