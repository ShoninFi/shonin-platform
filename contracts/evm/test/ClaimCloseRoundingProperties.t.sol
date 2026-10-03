// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Claim, ClaimState, IVault} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";
import {TwoVenueVault} from "./TwoVenueVault.sol";

/// Tiny claims against a station with no cash at home: the rounding of a
/// close must never leave a healthy claim unservable.
contract ClaimCloseWithLittleCashTest is ClaimBase {
    function zeroFloor() internal {
        vm.prank(author);
        vault.setIdleFloor(0);
        skip(1 days);
        blessExecutorKey(executor, address(vault), bundle);
    }

    /// A hedged account (short perp against spot) worth `venueUnits` at the
    /// mark with almost no venue cash; the vault holds one unit of cash.
    /// forge-config: default.fuzz.runs = 500
    function testFuzz_tiny_claims_on_a_hedged_account_are_served(
        uint256 venueUnits,
        uint256 shift,
        uint256 spreadBps,
        uint256 extraCash
    ) public {
        venueUnits = bound(venueUnits, 100, 100_000e6);
        shift = bound(shift, 0, 50);
        spreadBps = bound(spreadBps, 0, 199);
        extraCash = bound(extraCash, 0, 5);
        zeroFloor();
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(vault.cash() - 1);
        station.approveKey("local", executor);
        station.deposit("local", venueUnits);
        // Short nine times the margin, buy spot with the cash that gives.
        uint256 shortSize = venueUnits * 9 * 1e18 / PRICE;
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: false, size: shortSize, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        uint256 cashNow = uint256(venue.cashOf(address(station)));
        if (cashNow > extraCash) {
            orders[0] = OrderRequest({
                market: SPOT,
                isBuy: true,
                size: (cashNow - extraCash) * 1e18 / PRICE,
                limitPrice: PRICE,
                reduceOnly: false
            });
            venue.place(address(station), orders);
        }
        vm.stopPrank();
        setPrice(SPOT, PRICE, spreadBps);
        setPrice(PERP, PRICE, spreadBps);
        assertFalse(station.outsideBand());
        value(vault);
        uint256 shares = bobShares >> shift;
        if (shares == 0) shares = 1;
        vm.prank(bob);
        vault.requestRedeem(shares, bob);
        value(vault);
        uint256 id = vault.claimCount();
        if (id == 0 || vault.claimAt(id).state != ClaimState.Pending) return;
        uint256 bobWorth = worth(shares);
        (, uint256 exitBefore) = driver.valueOf(address(station));
        vm.prank(carol);
        (bool ok, bytes memory reason) = address(vault).call(abi.encodeCall(IVault.settleClaim, (id)));
        emit log_named_uint("venue exit before", exitBefore);
        emit log_named_uint("shares", shares);
        emit log_named_uint("bob worth", bobWorth);
        if (!ok) emit log_named_bytes("settleClaim reverted", reason);
        assertTrue(ok, "settleClaim reverted on a healthy hedged account");
        Claim memory c = vault.claimAt(id);
        assertTrue(c.state == ClaimState.Paid || c.state == ClaimState.Queued, "a healthy claim did not pay");
        if (c.state == ClaimState.Paid) {
            assertLe(usd.balanceOf(bob), bobWorth + 1, "paid more than the fraction");
        }
    }

    /// A plain spot account of any size with no venue cash and no cash at home.
    /// forge-config: default.fuzz.runs = 500
    function testFuzz_tiny_claims_on_a_spot_account_are_served(
        uint256 venueUnits,
        uint256 shift,
        uint256 spreadBps
    ) public {
        venueUnits = bound(venueUnits, 1, 150_000e6);
        shift = bound(shift, 0, 50);
        spreadBps = bound(spreadBps, 0, 199);
        zeroFloor();
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(vault.cash() - 1);
        station.approveKey("local", executor);
        station.deposit("local", venueUnits);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] = OrderRequest({
            market: SPOT,
            isBuy: true,
            size: venueUnits * 1e18 / PRICE,
            limitPrice: PRICE,
            reduceOnly: false
        });
        venue.place(address(station), orders);
        vm.stopPrank();
        setPrice(SPOT, PRICE, spreadBps);
        value(vault);
        uint256 shares = bobShares >> shift;
        if (shares == 0) shares = 1;
        vm.prank(bob);
        vault.requestRedeem(shares, bob);
        value(vault);
        uint256 id = vault.claimCount();
        if (id == 0 || vault.claimAt(id).state != ClaimState.Pending) return;
        uint256 bobWorth = worth(shares);
        vm.prank(carol);
        (bool ok, bytes memory reason) = address(vault).call(abi.encodeCall(IVault.settleClaim, (id)));
        if (!ok) emit log_named_bytes("settleClaim reverted", reason);
        assertTrue(ok, "settleClaim reverted on a healthy spot account");
        Claim memory c = vault.claimAt(id);
        assertTrue(c.state == ClaimState.Paid || c.state == ClaimState.Queued, "a healthy claim did not pay");
        if (c.state == ClaimState.Paid) {
            assertLe(usd.balanceOf(bob), bobWorth + 1, "paid more than the fraction");
        }
    }
}

/// Two venues: one healthy, the other holding a few units; claims and
/// serves of any size always go through.
contract ClaimCloseAtTwoVenuesTest is TwoVenueVault {
    /// forge-config: default.fuzz.runs = 400
    function testFuzz_a_few_units_at_the_second_venue_never_hold_a_claim(
        uint256 small,
        uint256 shift,
        uint256 spreadBps,
        bool asSpot
    ) public {
        small = bound(small, 0, 40);
        shift = bound(shift, 0, 50);
        spreadBps = bound(spreadBps, 0, 199);
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(vault.cash() - 1);
        station.approveKey("local", executor);
        station.approveKey("local2", executor);
        station.deposit("local", 100_000e6);
        if (small != 0) station.deposit("local2", small);
        vm.stopPrank();
        buySpot(venue, 1e18);
        if (asSpot && small != 0) buySpot(venue2, small * 1e18 / PRICE);
        spread(venue, spreadBps);
        spread(venue2, spreadBps);
        assertFalse(station.outsideBand());
        value(vault);
        uint256 shares = aliceShares >> shift;
        if (shares == 0) shares = 1;
        vm.prank(alice);
        vault.requestRedeem(shares, alice);
        value(vault);
        uint256 id = vault.claimCount();
        if (id == 0 || vault.claimAt(id).state != ClaimState.Pending) return;
        vm.prank(bob);
        (bool ok, bytes memory reason) = address(vault).call(abi.encodeCall(IVault.settleClaim, (id)));
        if (!ok) emit log_named_bytes("settleClaim reverted", reason);
        assertTrue(ok, "settleClaim reverted");
        Claim memory c = vault.claimAt(id);
        assertTrue(c.state == ClaimState.Paid || c.state == ClaimState.Queued, "a healthy claim did not pay");
    }

    /// forge-config: default.fuzz.runs = 400
    function testFuzz_a_few_units_at_the_second_venue_never_hold_a_serve(
        uint256 small,
        uint256 shift,
        uint256 spreadBps,
        bool asSpot
    ) public {
        small = bound(small, 0, 40);
        shift = bound(shift, 0, 50);
        spreadBps = bound(spreadBps, 0, 199);
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(vault.cash() - 1);
        station.approveKey("local", executor);
        station.approveKey("local2", executor);
        station.deposit("local", 100_000e6);
        if (small != 0) station.deposit("local2", small);
        vm.stopPrank();
        buySpot(venue, 1e18);
        if (asSpot && small != 0) buySpot(venue2, small * 1e18 / PRICE);
        spread(venue, spreadBps);
        spread(venue2, spreadBps);
        value(vault);
        uint256 shares = aliceShares >> shift;
        if (shares == 0) shares = 1;
        vm.prank(alice);
        uint256 req = vault.requestRedeem(shares, alice);
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRata(req);
        (uint256 held,) = vault.unitsOf(alice);
        if (held == 0) return;
        (, uint256 exit) = station.value();
        uint256 fair = exit * held / WAD;
        vm.prank(bob);
        (bool ok, bytes memory reason) = address(vault).call(abi.encodeCall(IVault.serveProRata, (alice)));
        if (!ok) emit log_named_bytes("serveProRata reverted", reason);
        assertTrue(ok, "serveProRata reverted");
        (uint256 after_, uint256 claimable) = vault.unitsOf(alice);
        assertEq(after_, 0, "units left");
        if (fair > 12) assertApproxEqAbs(claimable, fair, 12, "served other than the worth");
    }
}
