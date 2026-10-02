// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState, IVault} from "../src/interfaces/IVault.sol";
import {FaultingDriver} from "./FaultingDriverEscape.t.sol";
import {Fixture} from "./Fixture.sol";

/// A claim the cash covered, queued to be priced, takes the fallback like
/// any other request when nothing can be priced, and is paid its fraction.
contract QueuedClaimFallbackTest is Fixture {
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

    function test_a_queued_claim_can_escape_when_nothing_can_be_priced() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        hedge(190_000e6);

        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        // The strategy brings its idle dollars home: the cash covers her now.
        vm.startPrank(executor);
        station.withdraw("local", 100_000e6);
        station.push(100_000e6);
        vm.stopPrank();
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);
        uint256 claimRequest = vault.requestCount();

        // The venue pauses: its driver's reads revert, no valuation can be
        // recorded, and the fix takes the registry's timelock.
        FaultingDriver faulting = new FaultingDriver();
        vm.prank(admin);
        attestation.proposeDriver("local", address(faulting));
        skip(TIMELOCK);
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vm.expectRevert();
        vault.recordValuation();

        // Bob leaves through the fallback: half the cash, and units for half
        // the station.
        vm.prank(bob);
        uint256 bobRequest = vault.requestRedeem(bobShares, bob);
        skip(24 hours + SLOT);
        uint256 cash = vault.cash();
        vm.prank(bob);
        vault.takeProRata(bobRequest);
        // Half the cash each, less the transaction fee kept in the pool.
        uint256 half = cash / 2 - (cash / 2) * 10 / 10_000;
        assertApproxEqRel(usd.balanceOf(bob), half, 0.001e18);

        // Alice's queued claim takes the fallback too, through its own door.
        vm.prank(alice);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.takeProRata(claimRequest);
        // Hers is the rest of the cash, bob's fee included, less her own.
        uint256 rest = vault.cash();
        vm.prank(alice);
        vault.takeProRataClaim(1);
        assertApproxEqRel(
            usd.balanceOf(alice), rest - rest * 10 / 10_000, 0.001e18, "a queued claim had no way out"
        );
        (uint256 aliceUnits,) = vault.unitsOf(alice);
        assertGt(aliceUnits, 0, "she was given no part of the station");
    }
}
