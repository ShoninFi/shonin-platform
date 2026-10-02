// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Valuation} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// A tank fill the cash only partly covers is scaled down to the cash, and
/// the rounding of the scaled figure can never take more than is there, so a
/// fill can never stop the queue.
contract FillNeverExceedsCashTest is Fixture {
    using Math for uint256;

    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    /// `Valuations.exitOf`, as written.
    function exitOf(Valuation memory v, uint256 shares) internal view returns (uint256 payout) {
        uint256 slice = v.feeExit == 0 ? 0 : shares.mulDiv(v.feeExit, v.grossExit, Math.Rounding.Ceil);
        uint256 gross = (shares - slice).mulDiv(v.grossExit, v.supply);
        payout = gross - gross.mulDiv(vault.fees().transactionBps, 10_000, Math.Rounding.Ceil);
    }

    function testFuzz_a_short_tank_fill_never_pays_more_than_the_cash(uint256 short) public {
        depositAs(vault, author, 100_000e6);
        depositAs(vault, alice, 100_000e6);

        // The author asks for a fill worth about 60k; the vault holds 200k.
        uint256 shares = vault.balanceOf(author) * 6 / 10;
        vm.prank(author);
        vault.topUpFromShares(shares);

        // Leave the vault just short of what the fill would pay, by any amount
        // up to 40k units, so the fill is scaled down to the cash.
        Valuation memory v = vault.valuationAt(vault.valuationCount() - 1);
        uint256 payout = exitOf(v, shares);
        uint256 cash = payout - bound(short, 1, 40_000);
        uint256 surplus = vault.cash() - cash;
        vm.prank(executor);
        station.pull(surplus);
        assertEq(vault.cash(), cash);

        // It settles for no more than the cash, and the queue moves on.
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        vault.settle(1);
        assertLe(vault.tank().balance - MINIMUM_TANK + DEAD_ASSETS, cash, "the fill paid more than the cash");
        vm.warp(vm.getBlockTimestamp() + SLOT);
        vault.recordValuation();
    }
}
