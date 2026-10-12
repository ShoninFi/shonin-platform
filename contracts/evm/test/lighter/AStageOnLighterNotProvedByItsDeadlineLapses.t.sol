// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Stage} from "../../src/interfaces/IStation.sol";
import {ClaimState} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";

/// On Lighter a stage is proved by a post read after its settle delay, and
/// only before its deadline, two windows after it was placed: after it the
/// agent may trade the accounts again. A proof sent after the deadline
/// lapses the stage with nothing paid, even on a post read before it.
contract AStageOnLighterNotProvedByItsDeadlineLapsesTest is LighterVault {
    function test_a_stage_on_lighter_not_measured_by_its_deadline_lapses_and_pays_nothing() public {
        uint256 aliceShares = _fundAndTrade();
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        require(vault.claimAt(1).state == ClaimState.Closing, "staged");
        Stage memory staged = station.stage();
        assertEq(staged.provedBy, staged.stagedAt + 2 * lighter.DELIVERY_WINDOW(), "two windows");
        _nextBlock();
        exchange.process();
        // A post that could prove it, read before the deadline, and no
        // proof sent until the deadline.
        skip(lighter.SETTLE_DELAY());
        _post();
        vm.warp(staged.provedBy);
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "the stage has ended");
        assertEq(exchange.withdrawalCount(), 0, "nothing was asked for");
        assertEq(usd.balanceOf(alice), 0, "nothing was paid");
        assertTrue(vault.claimAt(1).state != ClaimState.Paid, "her claim is still owed");
    }
}
