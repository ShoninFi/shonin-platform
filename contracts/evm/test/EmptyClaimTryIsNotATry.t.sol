// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A claim try that closes inside the band but brings nothing home serves
/// nobody: the claim waits, and the freeze clock keeps running.
contract EmptyClaimTryIsNotATryTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function buyPerp(uint256 size) internal {
        vm.startPrank(executor);
        station.approveKey("local", executor);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: size, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
    }

    function test_a_levered_strategy_holds_a_leaver_without_ever_freezing() public {
        uint256 aliceShares = depositAs(vault, alice, 20_000e6);
        depositAs(vault, bob, 180_000e6);
        // 190k at the venue, ten BTC long on a perp: 600k notional against
        // 190k, inside the venue's 10% margin, with the account's cash at
        // -410k.
        vm.startPrank(executor);
        station.pull(190_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 190_000e6);
        vm.stopPrank();
        buyPerp(10e18);
        assertLt(venue.cashOf(address(station)), 0);

        // Alice's withdrawal can't be paid from the 10k of cash: a claim.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);

        // A try either finds the market outside the band, or brings the
        // leaver's fraction home.
        skip(6 days);
        blessExecutorKey(executor, address(vault), bundle);
        uint256 cashBefore = vault.cash();
        vault.settleClaim(1);
        bool waiting = vault.claimAt(1).state == ClaimState.Waiting;
        assertTrue(waiting || vault.cash() > cashBefore, "a try inside the band brought nothing home");
    }
}
