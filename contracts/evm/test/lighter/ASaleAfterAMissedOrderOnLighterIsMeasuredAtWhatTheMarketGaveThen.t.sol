// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../../src/interfaces/IVault.sol";
import {Valuation} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";

/// On Lighter as on Hyperliquid, a position that shrank after the leaver's
/// order missed counts as her close; its least is priced at the worse of the
/// stage's index and the newest post's, so the agent's sale after a fall is
/// measured at what the market gave then.
contract ASaleAfterAMissedOrderOnLighterIsMeasuredAtWhatTheMarketGaveThenTest is LighterVault {
    function _worth(address who) internal view returns (uint256) {
        Valuation memory v = vault.valuationAt(vault.valuationCount() - 1);
        return vault.balanceOf(who) * (v.gross - 1) / v.supply;
    }

    function _run(bool missed) internal returns (uint256 paid, uint256 bobLoss, uint256 part) {
        uint256 aliceShares = _fundAndTrade();
        value(vault);
        uint256 bobBefore = _worth(bob);
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        require(vault.claimAt(1).state == ClaimState.Closing, "staged");
        _nextBlock();
        uint32 dropped = uint32(uint256(ETH_PX) * 95 / 100);
        if (missed) {
            // The mark is past her limit when the exchange runs her order.
            exchange.setMark(ETH, uint32(uint256(ETH_PX) * 985 / 1000));
            exchange.process();
            // ETH falls 5%; the agent's key sells 21 ETH at the market (a little more than her 20).
            exchange.setMark(ETH, dropped);
            (uint48 master,) = lighter.masterOf(address(station));
            vm.prank(address(station));
            exchange.createOrder(master, ETH, 210_000, 1, 1, 0);
            exchange.process();
        } else {
            exchange.process();
            exchange.setMark(ETH, dropped);
        }
        skip(lighter.SETTLE_DELAY());
        _post();
        station.settleClose();
        part = station.stage().part;
        exchange.process();
        _collect();
        _nextBlock();
        station.settleClose();
        paid = usd.balanceOf(alice);
        skip(lighter.SETTLE_DELAY());
        _post();
        value(vault);
        uint256 bobAfter = _worth(bob);
        bobLoss = bobBefore > bobAfter ? bobBefore - bobAfter : 0;
    }

    function test_the_agents_sale_after_a_missed_order_is_measured_at_the_fall() public {
        uint256 start = vm.snapshotState();
        (uint256 filled, uint256 bobFilled,) = _run(false);
        vm.revertToState(start);
        (uint256 missed, uint256 bobMissed, uint256 part) = _run(true);
        emit log_named_decimal_uint("paid, her order filled, then ETH -5%", filled, 6);
        emit log_named_decimal_uint("paid, her order missed, ETH -5%, the agent sold", missed, 6);
        emit log_named_decimal_uint("part proved (missed)", part, 18);
        emit log_named_decimal_uint("Bob's loss, filled", bobFilled, 6);
        emit log_named_decimal_uint("Bob's loss, missed", bobMissed, 6);
        assertLe(
            bobMissed, bobFilled + 100e6, "the stayer paid the leaver the stage's price for the pool's sale"
        );
    }
}
