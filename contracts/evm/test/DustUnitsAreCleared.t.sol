// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Units worth less than one unit of the dollar are cleared by a serve that
/// brings nothing home, so dust taken through the fallback can never lock
/// the executor out of the vault's cash.
contract DustUnitsAreClearedTest is ClaimBase {
    function test_dust_units_are_cleared_and_never_block_a_pull() public {
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(100_000e6, 100_000e6, 1.5e18);

        // Bob asks to redeem a sliver of shares: a fraction of the station
        // worth a tenth of one unit of the dollar.
        uint256 dust = vault.totalSupply() / 1e12;
        vm.prank(bob);
        vault.requestRedeem(dust, bob);
        // The slot that prices it finds the market wide, so it is a claim.
        setPrice(SPOT, PRICE, 2000);
        value(vault);
        setPrice(SPOT, PRICE, 0);
        assertEq(vault.claimCount(), 1, "the dust redeem was not booked as a claim");

        // A day on, he takes the claim's fallback and holds dust units.
        skip(24 hours);
        vault.settleClaim(1);
        vm.prank(bob);
        vault.takeProRataClaim(1);
        (uint256 units,) = vault.units();
        assertGt(units, 0, "no units were minted");

        // Worth nothing, they are cleared by a serve that brings nothing
        // home, and the executor can deploy again.
        vault.serveProRata(bob);
        (units,) = vault.units();
        assertEq(units, 0, "the dust units were not cleared");
        value(vault);
        vm.prank(executor);
        station.pull(1000e6);
    }
}
