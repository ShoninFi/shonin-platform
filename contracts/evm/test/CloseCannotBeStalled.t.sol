// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {IVault, VaultState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// Once a halted vault's station has been found empty it is ready to close,
/// and new withdrawal requests are refused: a leaver redeems once it has
/// closed, fee-free, so nobody can hold the close off with a request every
/// slot.
contract CloseCannotBeStalledTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function test_a_killed_vault_ready_to_close_takes_no_new_requests() public {
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 10e6);

        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);

        // A valuation strictly after the halt shows the station empty.
        value(vault);
        assertTrue(vault.state() == VaultState.Killed);

        // Bob, with a few share units, tries to slip in a one-unit request
        // before the close: it is refused, and the vault closes.
        vm.prank(bob);
        vm.expectRevert(IVault.WrongState.selector);
        vault.requestRedeem(1, bob);
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);

        // And he leaves fee-free from the closed vault.
        uint256 shares = vault.balanceOf(bob);
        vm.prank(bob);
        vault.redeemClosed(shares, bob);
        assertApproxEqRel(usd.balanceOf(bob), 10e6, 0.01e18);
    }
}
