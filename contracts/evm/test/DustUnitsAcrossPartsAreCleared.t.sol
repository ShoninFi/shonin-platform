// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A station rounds each venue's part and its idle part of a serve down on
/// its own, so units worth fewer dollar-units than the station has parts may
/// bring nothing home however often they are served. They are dust: a serve
/// clears them, the station can deploy again, and nobody can freeze the vault
/// for them.
contract DustUnitsAcrossPartsAreClearedTest is ClaimBase {
    function test_units_worth_a_unit_or_two_split_across_parts_are_cleared() public {
        depositAs(vault, alice, 100_000e6);
        // 40,000 idle at the station, 50,000 at the venue (20,000 cash and
        // 0.5 BTC): two buckets.
        deploySpot(90_000e6, 50_000e6, 0.5e18);
        uint256 bobShares = depositAs(vault, bob, 10e6);

        (, uint256 stations) = station.value();
        assertEq(stations, 90_000e6);
        uint256 supply = vault.totalSupply() + 1e12;
        // A fraction of the station worth about 1.5 units of the dollar:
        // the idle bucket's part (0.67) and the venue's part (0.83) each
        // round to nothing, the whole (1.5) does not.
        uint256 fraction = 1.5e18 / stations;
        uint256 shares = fraction * supply / 1e18 + 1;
        assertLt(shares, bobShares);

        // The market sits outside the band for a day: nothing prices bob's
        // redeem, and he takes the fallback.
        vm.prank(bob);
        uint256 id = vault.requestRedeem(shares, bob);
        setPrice(SPOT, PRICE, 2000);
        value(vault);
        skip(1 days);
        vm.prank(bob);
        vault.takeProRata(id);
        setPrice(SPOT, PRICE, 0);
        (uint256 held,) = vault.unitsOf(bob);
        assertGt(held, 0, "bob holds no units");
        assertEq(stations * held / 1e18, 1, "the units are not worth one dollar-unit");

        // Sound: the market is back, so a serve pays or clears the units.
        // Current code: NotYet, however often it is tried.
        vault.serveProRata(bob);
        (uint256 units,) = vault.units();
        assertEq(units, 0, "the units were not cleared");

        // Sound: the strategy can deploy the vault's cash again.
        value(vault);
        vm.prank(executor);
        station.pull(1000e6);

        // Sound: nobody can freeze a vault whose units were served.
        skip(7 days);
        vm.prank(alice);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.freezeUnits(bob);
        assertTrue(vault.state() == VaultState.Open, "the vault was frozen for served units");
    }
}
