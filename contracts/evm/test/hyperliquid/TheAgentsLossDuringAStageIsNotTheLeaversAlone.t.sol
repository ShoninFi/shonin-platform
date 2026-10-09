// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Stage} from "../../src/interfaces/IStation.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// The measure reprices what the account held at the stage, so a market
/// move on it cancels; it can't reprice what the agent trades after the
/// stage, nor tell the agent's fills from the leaver's. So each driver's
/// measure is floored at what her close fetched at the least: her part of
/// the account with every order filled at its limit from the stage's own
/// prices. A loss the pool's trading makes after her close was placed costs
/// her no more than the band on her own orders, whoever chooses when the
/// close is proved.
contract TheAgentsLossDuringAStageIsNotTheLeaversAloneTest is HyperliquidVault {
    function _measured(function() internal between) internal returns (Stage memory) {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        _nextBlock();
        between();
        skip(hl.SETTLE_DELAY());
        station.settleClose();
        return station.stage();
    }

    function _nothingButAMove() internal {
        _spread(ETH_PX * 101 / 100, 10);
    }

    function _agentSellsThenTheMarketRises() internal {
        core.trade(address(station), ETH, -100_000, ETH_PX);
        _spread(ETH_PX * 101 / 100, 10);
    }

    function test_the_agents_loss_after_the_stage_costs_the_leaver_no_more_than_the_band_on_her_orders()
        public
    {
        uint256 start = vm.snapshotState();
        Stage memory plain = _measured(_nothingButAMove);
        vm.revertToState(start);
        Stage memory traded = _measured(_agentSellsThenTheMarketRises);
        emit log_named_decimal_uint("fetched, no trade", plain.fetched, 6);
        emit log_named_decimal_uint("fetched, the agent's 10 ETH then +1%", traded.fetched, 6);
        emit log_named_decimal_uint("part, no trade", plain.part, 18);
        emit log_named_decimal_uint("part, traded", traded.part, 18);
        // At most her share (a half) of the pool's $270 loss; fairly none of
        // it, since her positions were already being sold.
        // No more of the pool's $270 loss than the band on her own orders,
        // about $990 here (1% and a fill's fee bound of 0.1% on her 33.3 ETH at the stage's price).
        assertGe(
            traded.fetched + 1000e6,
            plain.fetched,
            "the leaver paid more than the band and a fill's fee on her orders"
        );
    }

    /// The other side: the same trade with a move the agent gains on is
    /// cut off at the estimate, so the leaver never gets it.
    function test_and_the_matching_gain_is_not_hers() public {
        uint256 start = vm.snapshotState();
        Stage memory plain = _measured(_nothingButAMove);
        vm.revertToState(start);
        Stage memory gained = _measured(_agentSellsThenTheMarketFallsBack);
        emit log_named_decimal_uint("fetched, no trade", plain.fetched, 6);
        emit log_named_decimal_uint("fetched, the agent's 10 ETH then -1%", gained.fetched, 6);
        emit log_named_decimal_uint("estimate", gained.estimate, 6);
        // Shown, not asserted: the gain stops at the estimate.
        assertLe(gained.fetched, gained.estimate);
    }

    function _agentSellsThenTheMarketFallsBack() internal {
        core.trade(address(station), ETH, -100_000, ETH_PX);
        _spread(ETH_PX * 99 / 100, 10);
    }

    function _agentSellsALotThenTheMarketRises() internal {
        // 100 ETH sold at the mark after her close, then ETH up 5% before
        // the proof: the pool loses $13,500 on the agent's trade.
        core.trade(address(station), ETH, -1_000_000, ETH_PX);
        _spread(ETH_PX * 105 / 100, 10);
    }

    function _nothingButABigMove() internal {
        _spread(ETH_PX * 105 / 100, 10);
    }

    /// However large the pool's loss on the agent's trading after the stage,
    /// the leaver bears no more of it than the band on her own orders.
    function test_a_large_loss_after_the_stage_costs_the_leaver_no_more_than_the_band_on_her_orders()
        public
    {
        uint256 start = vm.snapshotState();
        Stage memory plain = _measured(_nothingButABigMove);
        vm.revertToState(start);
        Stage memory traded = _measured(_agentSellsALotThenTheMarketRises);
        emit log_named_decimal_uint("fetched, no trade", plain.fetched, 6);
        emit log_named_decimal_uint("fetched, the agent's 100 ETH then +5%", traded.fetched, 6);
        // The pool lost $13,500; the band and a fill's fee on her orders are about $990 at the
        // stage's price, and the rise since is not hers to keep or pay.
        assertGe(
            traded.fetched + 1000e6,
            plain.fetched,
            "the leaver paid more than the band and a fill's fee on her orders"
        );
    }
}
