// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState, IVault, VaultState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A try that serves a claim and queues it restarts its freeze clock, so a
/// claim seconds from being paid can't freeze the vault.
contract QueuedTryRestartsTheClockTest is Fixture {
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

    function test_a_claim_served_and_queued_cannot_freeze_the_vault() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        hedge(90_000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);

        // Six days and change go by; then the claim is served inside the band
        // and queued: it will be paid at the next slot.
        skip(7 days - 1);
        blessExecutorKey(executor, address(vault), bundle);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);

        // A second later nobody can freeze the vault over it.
        skip(1);
        vm.expectRevert(IVault.NotYet.selector);
        vault.freeze(1);

        // It is paid at the next valuation.
        value(vault);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        assertTrue(vault.state() != VaultState.Frozen, "a served claim froze the vault");
    }
}
