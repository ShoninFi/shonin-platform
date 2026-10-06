// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ValuationStatus} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";

/// Each move of dollars through Lighter voids valuations until a post read
/// late enough shows it (a credit time after a deposit, a settle delay after
/// an ask), and the driver spaces the executor's moves from when such a post
/// is sure to exist, a whole slot and its window past it. So a strategy that
/// moves dollars as often as the driver lets it, with the executor posting
/// once a minute and the keeper valuing every slot, still leaves a priced
/// valuation between every two of its moves.
contract MovesThroughLighterLeaveRoomForAValuationTest is LighterVault {
    uint256 internal deposits;
    uint256 internal withdrawals;
    uint256 internal valued;
    uint256 internal pricedSince;
    uint256 internal unpricedGaps;

    /// Two hours of the executor posting a minute apart (five seconds after
    /// a minute's start), the strategy moving a dollar right after a post
    /// whenever the driver lets it (`withdrawing`: alternately out and in),
    /// and the keeper valuing every slot.
    function _run(bool withdrawing) internal {
        _fundAndTrade();
        vm.prank(executor);
        station.pull(100e6);

        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % 60) + 65);
        uint256 nextPost = vm.getBlockTimestamp();
        uint256 end = nextPost + 2 hours;
        bool posted;
        bool out;
        while (vm.getBlockTimestamp() < end) {
            uint256 now_ = vm.getBlockTimestamp();
            if (now_ >= nextPost) {
                _post();
                nextPost += 60;
                posted = true;
            } else if (posted) {
                posted = false;
                vm.prank(executor);
                if (withdrawing && out) {
                    try station.withdraw(LIGHTER, 1e6) {
                        _moved();
                        withdrawals++;
                        out = false;
                        exchange.process();
                    } catch {}
                } else {
                    try station.deposit(LIGHTER, 1e6) {
                        _moved();
                        deposits++;
                        out = true;
                    } catch {}
                }
            }
            if (deposits != 0 && now_ % SLOT == 0) {
                vault.recordValuation();
                vault.settle(type(uint256).max);
                valued++;
                if (vault.valuationAt(vault.valuationCount() - 1).status == ValuationStatus.Priced) {
                    pricedSince++;
                }
            }
            vm.roll(block.number + 1);
            skip(1);
        }
    }

    /// A move: the stretch since the last one must have priced a valuation.
    function _moved() internal {
        if (deposits + withdrawals != 0 && pricedSince == 0) unpricedGaps++;
        pricedSince = 0;
    }

    function test_a_strategy_depositing_as_often_as_the_driver_allows_leaves_a_priced_valuation_between_deposits(
    ) public {
        _run(false);
        assertGt(deposits, 5, "the driver let the strategy deposit about every quarter of an hour");
        assertGt(valued, 600, "the keeper valued every slot");
        assertEq(unpricedGaps, 0, "two deposits had no priced valuation between them");
    }

    function test_a_strategy_moving_dollars_in_and_out_as_often_as_allowed_leaves_a_priced_valuation_between_moves(
    ) public {
        _run(true);
        assertGt(withdrawals, 3, "the driver let the strategy withdraw");
        assertGt(deposits, 3, "and deposit");
        assertEq(unpricedGaps, 0, "two moves had no priced valuation between them");
    }
}
