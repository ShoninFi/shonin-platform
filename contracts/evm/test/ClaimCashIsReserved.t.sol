// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Claim, ClaimState, IVault} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// The cash a claim's close brings home is set aside for it until it is
/// priced: a request ahead of it can't take it, and nor can the station.
contract ClaimCashIsReservedTest is Fixture {
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

    function test_a_claim_whose_fraction_came_home_is_paid_from_it() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        // 190k deployed; 10k of cash left (the 5% floor).
        hedge(190_000e6);

        // Alice's withdrawal can't be paid from cash: a claim.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);
        assertTrue(vault.claimAt(1).state == ClaimState.Pending);

        // Bob queues a redemption, then serves alice's claim: her fraction
        // (about 95k) comes home and the claim is queued behind bob's request.
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        blessExecutorKey(executor, address(vault), bundle);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);
        assertGt(vault.cash(), 100_000e6, "alice's fraction came home");

        // The valuation reaches bob's request first, but alice's cash is set
        // aside: bob's request books a claim of its own, and alice is paid.
        value(vault);
        assertLt(usd.balanceOf(bob), 99_000e6, "bob was paid from the claim's cash");
        Claim memory c = vault.claimAt(1);
        assertTrue(c.state == ClaimState.Paid, "the claim's cash was taken by a redemption ahead of it");
    }

    /// Nothing stops the station taking the cash back out once the claim has
    /// fallen back to Pending, and bob can come back and do it again.
    function test_the_claims_cash_cannot_be_pulled_back_out() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        hedge(190_000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        blessExecutorKey(executor, address(vault), bundle);
        vault.settleClaim(1);
        // While it waits to be priced, the station can't touch the cash.
        vm.prank(executor);
        vm.expectRevert(IVault.WrongState.selector);
        station.pull(1);
        value(vault);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "alice's close bought her nothing");
    }
}
