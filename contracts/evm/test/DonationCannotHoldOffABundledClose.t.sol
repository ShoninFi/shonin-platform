// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {IVault, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A stranger who sends a stopped station a little over the dust before each
/// valuation holds off a keeper that unwinds, values and closes in separate
/// transactions, at about a dollar a round that ends with the holders. A
/// keeper that does all three in one transaction can't be got between.
contract DonationCannotHoldOffABundledCloseTest is ClaimBase {
    BundlingKeeper internal keeper;

    function setUp() public override {
        super.setUp();
        keeper = new BundlingKeeper();
    }

    function _nextSlot() internal {
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
    }

    function test_a_dollar_a_round_holds_off_an_unbundled_close_but_not_a_bundled_one() public {
        depositAs(vault, alice, 100_000e6);
        // 50k idle at the station, nothing at the venue.
        deploySpot(50_000e6, 0, 0);
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        station.unwindStopped(new bytes32[](0));
        assertEq(usd.balanceOf(address(station)), 0);

        // The unbundled keeper: a stranger's dollar lands before each
        // valuation, so each one finds the station holding more than dust.
        for (uint256 i = 0; i < 3; i++) {
            give(address(station), MINIMUM_DEPOSIT / 10 + 1);
            _nextSlot();
            vault.recordValuation();
            vault.settle(type(uint256).max);
            vm.expectRevert(IVault.NotYet.selector);
            vault.close();
            // The keeper sweeps it home, and the stranger does it again.
            station.unwindStopped(new bytes32[](0));
        }

        // Bundled: nothing can land between the sweep and the close.
        give(address(station), MINIMUM_DEPOSIT / 10 + 1);
        _nextSlot();
        keeper.sweepValueAndClose(vault, station);
        assertTrue(vault.state() == VaultState.Closed, "the bundled keeper could not close the vault");
    }
}

contract BundlingKeeper {
    function sweepValueAndClose(Vault vault, Station station) external {
        station.unwindStopped(new bytes32[](0));
        vault.recordValuation();
        vault.settle(type(uint256).max);
        vault.close();
    }
}
