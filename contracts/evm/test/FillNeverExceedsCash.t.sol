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

    function test_a_short_tank_fill_can_block_the_queue_for_good() public {
        depositAs(vault, author, 100_000e6);
        depositAs(vault, alice, 100_000e6);

        // The author asks for a fill worth about 60k; the vault holds 200k.
        uint256 shares = vault.balanceOf(author) * 6 / 10;
        vm.prank(author);
        vault.topUpFromShares(shares);

        // The valuation that prices it is recorded, not yet settled.
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        Valuation memory v = vault.valuationAt(vault.valuationCount() - 1);
        uint256 payout = exitOf(v, shares);

        // Look for a cash amount just under the payout for which the
        // scaled-down fill pays more than the cash.
        uint256 cash;
        bool found;
        for (uint256 d = 1; d < 40_000; d++) {
            cash = payout - d;
            uint256 scaled = shares.mulDiv(cash, payout);
            if (exitOf(v, scaled) > cash) {
                found = true;
                break;
            }
        }
        assertTrue(found, "no overshooting cash value within 40k units of the payout");

        // Put exactly that much cash in the vault (the 5% floor is ~10k).
        uint256 now_ = vault.cash();
        assertGt(now_, cash);
        vm.prank(executor);
        station.pull(now_ - cash);
        assertEq(vault.cash(), cash);

        // A fill the cash can't cover settles for what the cash covers, and
        // never stops the queue.
        vault.settle(1);
        assertLe(vault.tank().balance - MINIMUM_TANK + DEAD_ASSETS, cash, "the fill paid more than the cash");
        vm.warp(vm.getBlockTimestamp() + SLOT);
        vault.recordValuation();
    }
}
