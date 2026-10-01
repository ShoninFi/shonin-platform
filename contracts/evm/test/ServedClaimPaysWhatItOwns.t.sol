// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A claim whose fraction is already home is paid what it owns: what its
/// close brought home and its share of the rest of the cash, less its own
/// profit fee and the transaction fee. A loss the station takes after the
/// close is the stayers', as a gain would be.
contract ServedClaimPaysWhatItOwnsTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    /// One BTC long against one short, no spread: 190k at the venue.
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

    function test_a_served_claim_is_paid_what_it_owns_even_if_the_station_then_loses() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        // 190k deployed, 10k of cash (the 5% floor).
        hedge(190_000e6);

        // Alice (half the vault) leaves; the cash can't pay her: a claim.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);

        // Served: her half of the positions is closed and ~95k comes home.
        uint256 cashBefore = vault.cash();
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);
        uint256 home = vault.cash() - cashBefore;
        assertApproxEqAbs(home, 95_000e6, 100e6, "her half came home");

        // What she owns now, by the contract's own rule for a closed claim:
        // what her close brought home plus her share of the rest of the cash,
        // less the transaction fee. No profit fee: the pool is below its mark.
        uint256 owned = home + (vault.cash() - home) * aliceShares / vault.totalSupply();
        uint256 fair = owned - owned * vault.fees().transactionBps / 10_000;
        assertApproxEqAbs(fair, 100_000e6, 200e6);

        // Before the next slot the remaining spot leg halves in price: the
        // station is worth 15k less. Alice's money is already in cash.
        setPrice(SPOT, PRICE / 2, 0);
        (, uint256 stationExit) = station.value();
        assertApproxEqAbs(stationExit, 80_000e6, 100e6, "the station lost 15k after her close");

        // The next valuation prices the claim.
        value(vault);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        uint256 paid = usd.balanceOf(alice);
        emit log_named_uint("alice owned (after tx fee)", fair);
        emit log_named_uint("alice was paid", paid);

        // She is paid her share of a loss the station took on positions that
        // were no longer hers: about 7.5k less than she owned.
        assertApproxEqAbs(paid, fair, 50e6, "a served claim paid for a loss the stayers took after its close");
    }
}
