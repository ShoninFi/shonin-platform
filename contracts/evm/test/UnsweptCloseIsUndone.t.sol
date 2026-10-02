// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A claim's close that can't bring its worth home, because the venue keeps
/// the proceeds as margin, is undone: the claim waits, nothing is sold, and
/// trying it again and again sells nothing either.
contract UnsweptCloseIsUndoneTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    /// 150k at the venue and six BTC long on the perp: 360k of notional
    /// against 150k, inside the venue's 10% margin, with the account's cash
    /// at -210k.
    function leveredLong() internal {
        vm.startPrank(executor);
        station.pull(150_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 150_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 6e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        assertLt(venue.cashOf(address(station)), 0);
    }

    function test_a_claim_try_that_cannot_bring_cash_home_sells_nothing() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        leveredLong();

        // Alice's withdrawal can't be paid from the 50k of cash: a claim.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);

        // Closing her half inside the band would leave the account's cash
        // negative, so nothing could come home: the close is undone and the
        // claim waits.
        uint256 cashBefore = vault.cash();
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting);
        assertEq(vault.cash(), cashBefore, "the first try brought something home");
        int256 sizeAfterFirst = venue.positionOf(address(station), PERP).size;
        assertEq(sizeAfterFirst, 6e18, "a close that brought nothing home was kept");

        // A passer-by tries again, and sells nothing either.
        vault.settleClaim(1);
        int256 sizeAfterSecond = venue.positionOf(address(station), PERP).size;
        assertEq(sizeAfterSecond, sizeAfterFirst, "a second try closed more of the pool's position");
    }
}
