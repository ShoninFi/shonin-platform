// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Claim, ClaimState, IVault, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Less-trodden paths: a stopped station, a closed vault, a claim beside a
/// queued one, and the rounding edge where a venue's part of a claim is
/// exactly one dollar-unit.
contract ClaimCloseEdgePropertiesTest is ClaimBase {
    function zeroFloor() internal {
        vm.prank(author);
        vault.setIdleFloor(0);
        skip(1 days);
        blessExecutorKey(executor, address(vault), bundle);
    }

    function expectedPayout(uint256 shares) internal view returns (uint256) {
        uint256 owned = worth(shares);
        uint256 basis = ceilDiv(vault.mark() * shares, 1e30);
        uint256 fee = owned > basis ? (owned - basis) * 1500 / 10_000 : 0;
        uint256 gross = owned - fee;
        return gross - ceilDiv(gross * 10, 10_000);
    }

    /// A killed vault whose station has stopped still serves a claim by a
    /// fractional close, at the ordinary band, and pays the fraction.
    function test_q1_claim_served_on_a_stopped_station() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(150_000e6, 150_000e6, 2e18);
        setPrice(SPOT, PRICE, 100);
        value(vault);
        uint256 id = claimFor(bob, bobShares);
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE + 1);
        station.stop();
        assertTrue(station.stopped());
        // Sync the holding fee at this timestamp so the expectation matches.
        value(vault);
        uint256 expected = expectedPayout(bobShares);
        vm.prank(carol);
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid, "not paid on a stopped station");
        assertApproxEqAbs(usd.balanceOf(bob), expected, 4, "paid other than the fraction");
    }

    /// Units outstanding at a close: the closed vault still serves them for
    /// their worth, and the shares' redemption is unaffected.
    function test_q2_units_served_on_a_closed_vault() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(150_000e6, 150_000e6, 2e18);
        setPrice(SPOT, PRICE, 100);
        value(vault);
        // Alice takes the fallback.
        vm.prank(alice);
        uint256 req = vault.requestRedeem(aliceShares, alice);
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRata(req);
        (uint256 held,) = vault.unitsOf(alice);
        assertGt(held, 0);
        // Kill; the station is stopped and fully unwound by anyone.
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE + 1);
        station.stop();
        (, uint256 exitBefore) = station.value();
        uint256 aliceFair = exitBefore * held / WAD;
        bytes32[] memory names = new bytes32[](1);
        names[0] = "local";
        station.unwindStopped(names);
        value(vault);
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);
        (uint256 heldAfter, uint256 claimable) = vault.unitsOf(alice);
        emit log_named_uint("alice fair", aliceFair);
        emit log_named_uint("alice claimable", claimable);
        assertApproxEqAbs(claimable, aliceFair, 4, "the units were not paid their fraction of the unwind");
        // Bob redeems the closed vault's cash for his shares.
        uint256 bobFair = vault.cash() * bobShares / vault.totalSupply();
        vm.prank(bob);
        vault.redeemClosed(bobShares, bob);
        assertEq(usd.balanceOf(bob), bobFair);
        heldAfter;
    }

    /// Claim B is queued (the cash covers it at the last valuation); claim
    /// A is then closed; the next valuation pays B: each gets their fraction.
    function test_q3_a_close_beside_a_queued_claim_pays_both_their_fraction() public {
        uint256 aliceShares = depositAs(vault, alice, 200_000e6);
        uint256 bobShares = depositAs(vault, bob, 50_000e6);
        depositCarol(50_000e6);
        // 260k in spot, 40k cash: neither is covered; once 29k more comes
        // home bob's 50k is covered at the stale price and alice's is not.
        deploySpot(260_000e6, 260_000e6, 4e18);
        setPrice(SPOT, PRICE, 100);
        value(vault);
        uint256 a = claimFor(alice, aliceShares);
        uint256 b = claimFor(bob, bobShares);
        // Push 30k home: cash 110k covers one claim at the stale price.
        vm.startPrank(executor);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] = OrderRequest({market: SPOT, isBuy: false, size: 0.5e18, limitPrice: 0, reduceOnly: true});
        venue.place(address(station), orders);
        station.withdraw("local", 29_000e6);
        station.push(29_000e6);
        vm.stopPrank();
        vm.prank(carol);
        vault.settleClaim(b);
        assertTrue(vault.claimAt(b).state == ClaimState.Queued, "b not queued");
        uint256 aExpected = expectedPayout(aliceShares);
        vm.prank(carol);
        vault.settleClaim(a);
        assertTrue(vault.claimAt(a).state == ClaimState.Paid, "a not paid");
        assertApproxEqAbs(usd.balanceOf(alice), aExpected, 4, "a paid other than her fraction");
        // Alice's close took her cash share, so the cash no longer covers
        // bob at the valuation: he goes back to pending and is served by a
        // close for exactly his fraction.
        value(vault);
        assertTrue(vault.claimAt(b).state == ClaimState.Pending, "b not back to pending");
        uint256 bExpected = expectedPayout(bobShares);
        vm.prank(carol);
        vault.settleClaim(b);
        assertTrue(vault.claimAt(b).state == ClaimState.Paid, "b not paid");
        assertApproxEqAbs(usd.balanceOf(bob), bExpected, 4, "b paid other than his fraction");
    }

    /// A hedged account with no venue cash and a claim whose part of the
    /// venue at exit is exactly one unit: the rounding of the buy-back and
    /// the sale can leave the venue cash a unit short, and then the station
    /// sees "nothing home for a fraction worth something".
    /// forge-config: default.fuzz.runs = 2000
    function testFuzz_q4_one_unit_claims_on_a_hedged_account(
        uint256 venueUnits,
        uint256 shortBps,
        uint256 spreadBps,
        uint256 seed
    ) public {
        venueUnits = bound(venueUnits, 1000, 1_000_000);
        shortBps = bound(shortBps, 1000, 9000); // short notional as bps of... see below
        spreadBps = bound(spreadBps, 0, 199);
        zeroFloor();
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(vault.cash() - 1);
        station.approveKey("local", executor);
        station.deposit("local", venueUnits);
        uint256 shortSize = venueUnits * shortBps / 1000 * 1e18 / PRICE;
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: false, size: shortSize, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        uint256 cashNow = uint256(venue.cashOf(address(station)));
        orders[0] = OrderRequest({
            market: SPOT,
            isBuy: true,
            size: cashNow * 1e18 / PRICE,
            limitPrice: PRICE,
            reduceOnly: false
        });
        venue.place(address(station), orders);
        vm.stopPrank();
        setPrice(SPOT, PRICE, spreadBps);
        setPrice(PERP, PRICE, spreadBps);
        value(vault);
        (, uint256 exit) = driver.valueOf(address(station));
        if (exit < 2) return;
        // Shares whose fraction of the venue's exit value is one unit.
        uint256 supply = vault.totalSupply() + 1e12;
        uint256 shares = supply * (1 + seed % 2) / exit; // part = 1 or 2 units
        shares = shares + (seed % 7) - 3;
        if (shares == 0 || shares > bobShares) return;
        vm.prank(bob);
        vault.requestRedeem(shares, bob);
        value(vault);
        uint256 id = vault.claimCount();
        if (id == 0 || vault.claimAt(id).state != ClaimState.Pending) return;
        uint256 part = exit * shares / (vault.totalSupply() + 1e12);
        vm.prank(carol);
        (bool ok, bytes memory reason) = address(vault).call(abi.encodeCall(IVault.settleClaim, (id)));
        Claim memory c = vault.claimAt(id);
        emit log_named_uint("venue exit", exit);
        emit log_named_uint("part", part);
        emit log_named_int("venue cash before", venue.cashOf(address(station)));
        if (!ok) emit log_named_bytes("reverted", reason);
        assertTrue(ok, "settleClaim reverted");
        assertTrue(
            c.state == ClaimState.Paid || c.state == ClaimState.Queued,
            "a one-unit part of a healthy hedged account held the claim"
        );
    }
}
