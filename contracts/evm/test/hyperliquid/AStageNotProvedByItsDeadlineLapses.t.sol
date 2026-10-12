// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Stage} from "../../src/interfaces/IStation.sol";
import {ClaimState} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// A staged close is proved at a fixed moment: from its settle delay (a
/// whole close) or its window (a part), and before its deadline, two
/// windows after it was placed. Until then the attested executor trades
/// nothing at the venue, so what the account did meanwhile is the stage's
/// own; after it the agent may trade again, nothing read there tells the
/// close from its trading, and the stage lapses with nothing paid. The
/// claim it was for is then staged again like any other.
contract AStageNotProvedByItsDeadlineLapsesTest is HyperliquidVault {
    function _staged() internal returns (Stage memory staged) {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        require(vault.claimAt(1).state == ClaimState.Closing, "staged");
        staged = station.stage();
        // Her orders fill.
        _nextBlock();
    }

    function test_the_deadline_is_two_windows_after_the_stage() public {
        Stage memory staged = _staged();
        assertEq(staged.provedBy, staged.stagedAt + 2 * hl.DELIVERY_WINDOW());
    }

    function test_a_stage_measured_just_before_its_deadline_is_measured() public {
        Stage memory staged = _staged();
        vm.warp(staged.provedBy - 1);
        station.settleClose();
        Stage memory after_ = station.stage();
        assertGt(after_.sweptBlock, 0, "measured and swept");
        assertEq(after_.part, 1e18, "her whole close");
    }

    function test_a_stage_not_measured_by_its_deadline_lapses_and_pays_nothing() public {
        Stage memory staged = _staged();
        vm.warp(staged.provedBy);
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "the stage has ended");
        assertEq(usd.balanceOf(alice), 0, "nothing was paid");
        assertTrue(vault.claimAt(1).state != ClaimState.Paid, "her claim is still owed");
        assertTrue(vault.claimAt(1).state != ClaimState.Closing, "and no longer closing");
    }

    function test_a_lapsed_stages_claim_can_be_staged_again() public {
        Stage memory staged = _staged();
        vm.warp(staged.provedBy);
        station.settleClose();
        require(station.stage().stagedAt == 0, "lapsed");
        // A valuation prices what the lapsed stage's fill left, then her
        // claim is tried again and its close staged anew.
        skip(hl.DELIVERY_WINDOW());
        value(vault);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Closing, "staged again");
        assertGt(station.stage().stagedAt, staged.stagedAt, "a new stage");
        _settleStaged();
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "she is paid");
        assertGt(usd.balanceOf(alice), 90_000e6, "about her half of the pool");
    }
}
