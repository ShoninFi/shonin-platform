// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IStation} from "../src/interfaces/IStation.sol";
import {IVault, Request, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Units worth a dollar-unit at the mark and nothing at the bid can't be
/// closed (a close would pay the holder nothing), so a serve that can't bring
/// them home judges them as dust and clears them: nobody can hold a vault's
/// cash or freeze it with them.
contract DustUnderwaterAtExitIsClearedTest is ClaimBase {
    /// Everything at the venue, as 1.5 BTC with a 1% spread; nothing idle at
    /// the station, so what the station holds is the venue account alone.
    function _deployed() internal {
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(150_000e6, 150_000e6, 2.5e18);
        setPrice(SPOT, PRICE, 100);
        assertEq(usd.balanceOf(address(station)), 0, "the station keeps something idle");
        value(vault);
    }

    /// The fraction window in which the station refuses a close: worth one
    /// unit at the mark and nothing at the bid.
    function _window() internal view returns (uint256 low, uint256 high) {
        (uint256 mark, uint256 exit) = station.value();
        low = ceilDiv(WAD, mark);
        high = ceilDiv(WAD, exit);
        assertLt(low, high, "no window: the spread is too narrow for this price");
    }

    /// Shares that, taken through the fallback, come to `fraction` of the
    /// station as units.
    function _sharesFor(uint256 fraction) internal view returns (uint256) {
        return ceilDiv(fraction * (vault.totalSupply() + 1e12), WAD) + 1;
    }

    function _takeUnits(address who, uint256 shares) internal returns (uint256 held) {
        vm.prank(who);
        uint256 id = vault.requestRedeem(shares, who);
        Request memory r = vault.requestAt(id);
        // Nothing is priced for a day.
        vm.warp(r.earliest + 1 days + 1);
        vm.prank(who);
        vault.takeProRata(id);
        (held,) = vault.unitsOf(who);
        assertGt(held, 0, "no units");
    }

    function test_dust_units_worth_a_unit_at_the_mark_and_nothing_at_the_bid_are_cleared_not_stranded()
        public
    {
        _deployed();
        (uint256 low, uint256 high) = _window();
        uint256 held = _takeUnits(alice, _sharesFor((low + high) / 2));
        (uint256 mark, uint256 exit) = station.value();
        assertEq(mark * held / WAD, 1, "the units are not worth exactly one unit at the mark");
        assertEq(exit * held / WAD, 0, "the units are worth something at the bid");
        emit log_named_uint("alice's units, as a fraction of the station (wei of 1e18)", held);

        // A serve clears dust units. Anyone serves her.
        vm.prank(bob);
        (bool ok, bytes memory reason) = address(vault).call(abi.encodeCall(IVault.serveProRata, (alice)));
        assertTrue(ok, string(abi.encodePacked("the serve reverted: ", vm.toString(reason))));
        (uint256 after_,) = vault.unitsOf(alice);
        assertEq(after_, 0, "dust units were not cleared");

        // ... and so can't freeze the vault.
        skip(7 days);
        vm.prank(bob);
        (bool froze,) = address(vault).call(abi.encodeCall(IVault.freezeUnits, (alice)));
        assertFalse(
            froze && vault.state() == VaultState.Frozen, "a few millionths of a dollar froze the vault"
        );
    }

    /// The contrast: units one notch larger, worth a unit at the bid too,
    /// are served and cleared.
    function test_dust_units_worth_a_unit_at_the_bid_are_cleared() public {
        _deployed();
        (, uint256 high) = _window();
        uint256 held = _takeUnits(alice, _sharesFor(high + 1000));
        (, uint256 exit) = station.value();
        assertEq(exit * held / WAD, 1);
        vm.prank(bob);
        vault.serveProRata(alice);
        (uint256 after_,) = vault.unitsOf(alice);
        assertEq(after_, 0, "dust units were not cleared");
    }
}
