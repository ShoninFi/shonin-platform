// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {Stage} from "../../src/interfaces/IStation.sol";
import {ClaimState, IVault, ValuationStatus} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// What a staged close fetched is measured at its proof: what the account is
/// worth then, less what the part it was to keep of the account as it was
/// at the stage is worth at the prices then, plus whatever came home from it
/// meanwhile; never below nothing and never above the fraction's worth at
/// exit-side prices when it was placed. And while a close is in progress
/// nothing else is priced, divided or tried.
contract StagedCloseIsMeasuredTest is HyperliquidVault {
    /// Staged, then filled at the bid in the next block; proved and measured
    /// after `between` has run. What it was measured at.
    function _measured(function() internal between) internal returns (Stage memory) {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        _nextBlock();
        between();
        station.settleClose();
        return station.stage();
    }

    function _nothing() internal {}

    function _agentSells() internal {
        // The agent sells 10 ETH at the mark, after the leaver's close and
        // before its proof. (A trade that grew a position the close shrank
        // would leave it unproved, and it would lapse.)
        core.trade(address(station), ETH, -100_000, ETH_PX);
    }

    function _marketRises() internal {
        _spread(ETH_PX * 105 / 100, 10);
    }

    function _marketFalls() internal {
        _spread(ETH_PX * 95 / 100, 10);
    }

    function test_an_agents_trade_between_the_stage_and_the_proof_is_not_charged_to_the_leaver() public {
        uint256 start = vm.snapshotState();
        uint256 plain = _measured(_nothing).fetched;
        vm.revertToState(start);
        uint256 traded = _measured(_agentSells).fetched;
        assertApproxEqAbs(traded, plain, 1, "a trade at the price now nets to nothing");
    }

    function test_a_market_move_between_the_stage_and_the_proof_cancels_out() public {
        uint256 start = vm.snapshotState();
        uint256 plain = _measured(_nothing).fetched;
        vm.revertToState(start);
        uint256 up = _measured(_marketRises).fetched;
        vm.revertToState(start);
        uint256 down = _measured(_marketFalls).fetched;
        assertApproxEqAbs(up, plain, 0.1e6, "the stayers' part moving up is not the leaver's");
        assertApproxEqAbs(down, plain, 0.1e6, "nor moving down");
    }

    function test_a_fill_better_than_the_estimate_is_capped_at_the_estimate() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        // The bid jumps above the mark, inside the band, before the order
        // fills: the close fetches more than its estimate.
        core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX * 1005 / 1000, ETH_PX * 1006 / 1000);
        _nextBlock();
        _spread(ETH_PX, 10);
        station.settleClose();
        Stage memory s = station.stage();
        assertEq(s.fetched, s.estimate, "never more than the fraction's worth at exit prices");
    }

    function test_a_valuation_while_a_close_is_in_progress_prices_nothing_and_the_next_one_prices() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        _aliceClaims(aliceShares);
        vault.settleClaim(1);

        value(vault);
        uint256 index = vault.valuationCount() - 1;
        assertTrue(vault.valuationAt(index).status == ValuationStatus.Void, "void while closing");
        // Nothing goes out to the station, and no fallback is taken.
        vm.prank(executor);
        vm.expectRevert(Station.NotYet.selector);
        station.pull(1e6);

        _settleStaged();
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        value(vault);
        index = vault.valuationCount() - 1;
        assertTrue(vault.valuationAt(index).status == ValuationStatus.Priced, "priced once it is delivered");
    }

    function test_a_stream_of_claims_leaves_a_valuation_between_every_two_closes() public {
        _fundAndTrade();
        _stockGas();
        // Bob splits his exit into two claims.
        uint256 bobShares = vault.balanceOf(bob);
        vm.startPrank(bob);
        vault.requestRedeem(bobShares / 2, bob);
        vault.requestRedeem(bobShares - bobShares / 2, bob);
        vm.stopPrank();
        value(vault);
        assertEq(vault.claimCount(), 2);

        vault.settleClaim(1);
        // One close at a time.
        vm.expectRevert(IVault.NotYet.selector);
        vault.settleClaim(2);
        _settleStaged();
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        // The next close waits for a valuation, so a run of small claims
        // can't hold everyone else's requests off.
        vm.expectRevert(IVault.NotYet.selector);
        vault.settleClaim(2);
        value(vault);
        vault.settleClaim(2);
        assertTrue(vault.claimAt(2).state == ClaimState.Closing);
    }

    function test_a_stopped_stations_full_unwind_takes_the_place_of_a_staged_close() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        _aliceClaims(aliceShares);
        uint256 cashBefore = vault.cash();
        vault.settleClaim(1);
        _nextBlock();
        station.settleClose();
        assertGt(station.stage().sweptBlock, 0, "proved");

        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);
        assertEq(station.stage().stagedAt, 0, "the full unwind took its place");
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "the claim waits, paid as the vault closes");
        assertGe(vault.cash(), cashBefore, "its part of the cash is back in the cash");
    }
}
