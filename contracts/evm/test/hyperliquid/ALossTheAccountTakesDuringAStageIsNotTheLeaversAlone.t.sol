// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Stage} from "../../src/interfaces/IStation.sol";
import {ClaimState} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// The measure reprices what the account held at the stage, so a market
/// move on it cancels. From the stage until its proof the attested executor
/// trades nothing at the venue, but a fill can still reach the account that
/// isn't hers (an order resting before the stage, filled before the
/// executor cancelled it, or a reduction the venue forces), and the account
/// can't tell it from her own. A position smaller than her orders could
/// have made it is not hers to be paid or charged: the stage lapses, the
/// leaver is paid nothing and charged nothing, and her claim waits for a
/// later stage. Neither the pool's loss nor its gain on that fill is hers.
contract ALossTheAccountTakesDuringAStageIsNotTheLeaversAloneTest is HyperliquidVault {
    /// Staged and filled; proved once its window is over, after `between`
    /// has run. What it was measured at, and her claim's shares at the
    /// stage.
    function _measured(function() internal between) internal returns (Stage memory, uint256 claimed) {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        claimed = vault.claimAt(1).shares;
        _nextBlock();
        between();
        skip(station.stage().window);
        station.settleClose();
        return (station.stage(), claimed);
    }

    function _nothingButAMove() internal {
        _spread(ETH_PX * 101 / 100, 10);
    }

    function _aFillSellsThenTheMarketRises() internal {
        core.trade(address(station), ETH, -100_000, ETH_PX);
        _spread(ETH_PX * 101 / 100, 10);
    }

    function _aFillSellsThenTheMarketFallsBack() internal {
        core.trade(address(station), ETH, -100_000, ETH_PX);
        _spread(ETH_PX * 99 / 100, 10);
    }

    function _aFillSellsALotThenTheMarketRises() internal {
        // 100 ETH sold at the mark after her close, then ETH up 5% before
        // the proof: the pool loses $13,500 on that fill.
        core.trade(address(station), ETH, -1_000_000, ETH_PX);
        _spread(ETH_PX * 105 / 100, 10);
    }

    /// The stage ended with nothing paid or charged, and her claim waits
    /// whole.
    function _assertLapsed(uint256 claimed) internal view {
        assertEq(station.stage().stagedAt, 0, "the stage has ended");
        assertEq(usd.balanceOf(alice), 0, "nothing paid");
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "her claim waits");
        assertEq(vault.claimAt(1).shares, claimed, "with all its shares: nothing charged");
        assertEq(core.queued(), 0, "nothing asked of the venue");
    }

    function test_a_loss_after_the_stage_on_a_fill_beyond_her_orders_is_not_hers() public {
        uint256 start = vm.snapshotState();
        (Stage memory plain,) = _measured(_nothingButAMove);
        assertEq(plain.part, 1e18, "without the fill, her close is proved whole");
        assertGt(plain.fetched, 0);
        vm.revertToState(start);
        (, uint256 claimed) = _measured(_aFillSellsThenTheMarketRises);
        _assertLapsed(claimed);
    }

    /// The other side: the same fill with a move the account gains on is
    /// not hers either.
    function test_and_the_matching_gain_is_not_hers() public {
        (, uint256 claimed) = _measured(_aFillSellsThenTheMarketFallsBack);
        _assertLapsed(claimed);
    }

    /// However large the pool's loss on a fill after the stage, the leaver
    /// bears none of it, and none of it is paid out of the pool.
    function test_a_large_loss_after_the_stage_is_not_hers() public {
        (, uint256 claimed) = _measured(_aFillSellsALotThenTheMarketRises);
        _assertLapsed(claimed);
    }
}
