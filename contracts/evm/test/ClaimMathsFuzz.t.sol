// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A claim paid in the transaction that closes its fraction, fuzzed under
/// price moves, spreads and leverage, with the leaver choosing when to
/// close: a round trip never returns more than went in, and no stayer's
/// value per share falls.
contract ClaimMathsFuzzTest is Fixture {
    uint256 internal constant WAD = 1e18;

    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function hedge(uint256 amount, uint256 size) internal {
        vm.startPrank(executor);
        station.pull(amount);
        station.approveKey("local", executor);
        station.deposit("local", amount);
        OrderRequest[] memory orders = new OrderRequest[](2);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: size, limitPrice: PRICE, reduceOnly: false});
        orders[1] =
            OrderRequest({market: PERP, isBuy: false, size: size, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
    }

    /// Unhedged spot long: the pool moves with the price.
    function spotLong(uint256 amount, uint256 size) internal {
        vm.startPrank(executor);
        station.pull(amount);
        station.approveKey("local", executor);
        station.deposit("local", amount);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: size, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
    }

    function worthExit(uint256 shares) internal view returns (uint256) {
        (, uint256 exit) = station.value();
        (uint256 units,) = vault.units();
        uint256 owned = vault.cash() + exit * (WAD - units) / WAD;
        return owned * shares / vault.totalSupply();
    }

    function worthMark(uint256 shares) internal view returns (uint256) {
        (uint256 mark,) = station.value();
        (uint256 units,) = vault.units();
        uint256 owned = vault.cash() + mark * (WAD - units) / WAD;
        return owned * shares / vault.totalSupply();
    }

    function claimFor(address who, uint256 shares) internal returns (uint256 claimId) {
        vm.prank(who);
        vault.requestRedeem(shares, who);
        value(vault);
        claimId = vault.claimCount();
        assertTrue(vault.claimAt(claimId).state == ClaimState.Pending, "no claim");
    }

    /// Invariant 3 through a claim: deposit, then leave through a claim at
    /// the same prices. Never more back than went in.
    function testFuzz_round_trip_through_a_claim_never_pays_more_than_went_in(
        uint96 amount,
        uint16 spreadBps,
        uint8 pullPct
    ) public {
        amount = uint96(bound(amount, 10e6, 10_000_000e6));
        spreadBps = uint16(bound(spreadBps, 0, 150));
        pullPct = uint8(bound(pullPct, 50, 95));
        depositAs(vault, bob, 100_000e6);
        uint256 aliceShares = depositAs(vault, alice, amount);
        uint256 total = vault.cash();
        uint256 size = (total * pullPct / 100) * 1e18 / PRICE;
        hedge(total * pullPct / 100, size);
        setPrice(SPOT, PRICE, spreadBps);
        setPrice(PERP, PRICE, spreadBps);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        if (vault.claimCount() == 0) {
            // Paid from cash at the valuation: the ordinary path.
            assertLe(usd.balanceOf(alice), amount, "a round trip paid more than went in");
            return;
        }
        uint256 id = vault.claimCount();
        uint256 bobBefore = worthMark(vault.balanceOf(bob));
        vault.settleClaim(id);
        if (vault.claimAt(id).state != ClaimState.Paid) return;
        assertLe(usd.balanceOf(alice), amount, "a round trip through a claim paid more than went in");
        assertGe(worthMark(vault.balanceOf(bob)) + 2, bobBefore, "the stayer paid for her exit");
    }

    /// The leaver picks the moment: a price move between the claim's booking
    /// and its close. Whatever the move, the stayer's per-share value at
    /// mark is where it was just before the close, or higher.
    function testFuzz_a_leaver_timing_the_close_cannot_move_the_stayers_price(
        uint16 moveBps,
        bool up,
        uint16 spreadBps,
        uint8 leverTenths
    ) public {
        moveBps = uint16(bound(moveBps, 0, 3000));
        spreadBps = uint16(bound(spreadBps, 0, 150));
        leverTenths = uint8(bound(leverTenths, 5, 30));
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        // Spot long worth leverTenths/10 of the money at the venue.
        uint256 atVenue = 190_000e6;
        uint256 notional = atVenue * leverTenths / 10;
        if (notional > atVenue) notional = atVenue; // spot can't be borrowed
        spotLong(atVenue, notional * 1e18 / PRICE);
        uint256 id = claimFor(alice, aliceShares);

        uint256 newPrice = up ? PRICE * (10_000 + moveBps) / 10_000 : PRICE * (10_000 - moveBps) / 10_000;
        setPrice(SPOT, newPrice, spreadBps);
        uint256 bobBefore = worthMark(bobShares);
        uint256 aliceWorth = worthExit(aliceShares);
        uint256 cashBefore = vault.cash();
        vault.settleClaim(id);
        if (vault.claimAt(id).state != ClaimState.Paid) return;
        assertLe(usd.balanceOf(alice), aliceWorth + 1, "paid above her exit-side worth");
        assertGe(worthMark(bobShares) + 2, bobBefore, "the stayer's price fell at the leaver's chosen moment");
        // The pool kept the transaction fee at least.
        assertGe(
            vault.cash() + worthMark(0), cashBefore - cashBefore * aliceShares / (aliceShares + bobShares) - 1
        );
    }

    /// Two claims, each choosing their moment at a different price: each is
    /// paid at most their fraction at that moment, and the third holder's
    /// mark value never falls.
    function testFuzz_two_leavers_at_two_prices(uint16 m1, uint16 m2) public {
        m1 = uint16(bound(m1, 8000, 12_000));
        m2 = uint16(bound(m2, 8000, 12_000));
        (address carol, uint256 carolKey) = makeAddrAndKey("carol");
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        bytes memory statement = statementBy(carolKey, address(vault), carol, vault.restrictedText());
        give(carol, 100_000e6);
        vm.startPrank(carol);
        usd.approve(address(vault), 100_000e6);
        vault.requestDeposit(100_000e6, carol, statement);
        vm.stopPrank();
        value(vault);
        uint256 carolShares = vault.balanceOf(carol);
        spotLong(285_000e6, 4e18);

        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        value(vault);
        assertEq(vault.claimCount(), 2);

        setPrice(SPOT, PRICE * m1 / 10_000, 10);
        uint256 carolBefore = worthMark(carolShares);
        uint256 aliceWorth = worthExit(aliceShares);
        vault.settleClaim(1);
        assertLe(usd.balanceOf(alice), aliceWorth + 1);
        assertGe(worthMark(carolShares) + 2, carolBefore, "carol paid for alice");

        setPrice(SPOT, PRICE * m2 / 10_000, 10);
        carolBefore = worthMark(carolShares);
        uint256 bobWorth = worthExit(bobShares);
        vault.settleClaim(2);
        assertLe(usd.balanceOf(bob), bobWorth + 1);
        assertGe(worthMark(carolShares) + 2, carolBefore, "carol paid for bob");
    }
}
