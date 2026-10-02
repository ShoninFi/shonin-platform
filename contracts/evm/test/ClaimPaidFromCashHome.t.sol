// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A claim the cash at home already covers is queued as it stands, with
/// nothing to close, and paid at the next valuation, even when the station
/// is empty.
contract ClaimPaidFromCashHomeTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function test_a_claim_the_cash_covers_is_paid_with_the_station_empty() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        // The executor deploys 190k; 10k of cash stays (the 5% floor).
        vm.prank(executor);
        station.pull(190_000e6);

        // Alice's withdrawal can't be paid from 10k: a claim.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);
        assertTrue(vault.claimAt(1).state == ClaimState.Pending);

        // The executor goes to cash: everything comes home. The vault now
        // holds 200k, twice what alice is owed.
        vm.prank(executor);
        station.push(190_000e6);
        assertEq(vault.cash(), 200_000e6 + DEAD_ASSETS);
        (uint256 mark,) = station.value();
        assertEq(mark, 0, "the station is empty");

        // Serving the claim queues it on the cash alone.
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);

        // The next valuation pays her.
        value(vault);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "a claim the cash covers was not paid");
        assertApproxEqRel(usd.balanceOf(alice), 100_000e6, 0.01e18);
    }
}
