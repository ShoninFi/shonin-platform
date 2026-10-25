// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Valuation} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A leaver's slice of the profit fee is rounded against them, toward the
/// pool: never below their exact share of it.
contract TheLeaversFeeSliceRoundsAgainstThemTest is ClaimBase {
    function test_the_leavers_profit_fee_slice_is_rounded_up() public {
        uint256 shares = depositAs(vault, alice, 1000e6);
        deploySpot(300e6, 300e6, 0.004e8);
        // A gain above the mark, so a profit fee is owed.
        setPrice(SPOT, PRICE * 12 / 10, 0);
        uint256 authorBefore = vault.balanceOf(author);
        uint256 platformBefore = vault.balanceOf(platform);
        uint256 part = shares / 5 + 7; // an awkward number
        vm.prank(alice);
        vault.requestRedeem(part, alice);
        value(vault);
        (, uint256 index,) = vault.lastPriced();
        Valuation memory v = vault.valuationAt(index);
        assertGt(v.feeExit, 0, "a fee is owed");
        uint256 slice =
            (vault.balanceOf(author) - authorBefore) + (vault.balanceOf(platform) - platformBefore);
        uint256 exact = part * v.feeExit / v.grossExit;
        uint256 ceil = (part * v.feeExit + v.grossExit - 1) / v.grossExit;
        assertGe(slice, exact, "never below the exact share");
        assertEq(slice, ceil, "rounded up: against the leaver, for the pool");
    }
}
