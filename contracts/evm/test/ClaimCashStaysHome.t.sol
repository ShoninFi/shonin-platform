// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState, IVault, Valuation} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A claim's cash stays at home while the claim waits to be priced: the
/// station can't take it back out, so a strategy that keeps itself fully
/// deployed can't keep a leaver waiting for ever.
contract ClaimCashStaysHomeTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function latest() internal view returns (Valuation memory) {
        return vault.valuationAt(vault.valuationCount() - 1);
    }

    function test_a_claim_being_priced_keeps_its_cash_at_home() public {
        // Alice is nearly the whole vault; the strategy deploys 90% of it.
        uint256 shares = depositAs(vault, alice, 100_000e6);
        vm.prank(executor);
        station.pull(90_000e6);
        vm.startPrank(executor);
        station.approveKey("local", executor);
        station.deposit("local", 90_000e6);
        OrderRequest[] memory orders = new OrderRequest[](2);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        orders[1] =
            OrderRequest({market: PERP, isBuy: false, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();

        // Her withdrawal can't be paid from cash: a claim.
        vm.prank(alice);
        vault.requestRedeem(shares, alice);
        value(vault);
        uint256 claimId = vault.claimCount();
        assertEq(claimId, 1);

        // Serving it brings her fraction home; while it waits to be priced,
        // the station can't take that cash back out.
        vault.settleClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Queued);
        blessExecutorKey(executor, address(vault), bundle);
        vm.prank(executor);
        vm.expectRevert(IVault.WrongState.selector);
        station.pull(1);

        // So the next valuation pays her.
        value(vault);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Paid);
        assertGt(usd.balanceOf(alice), 99_000e6);

        // And the station may deploy again.
        vm.prank(executor);
        station.pull(1);
    }
}
