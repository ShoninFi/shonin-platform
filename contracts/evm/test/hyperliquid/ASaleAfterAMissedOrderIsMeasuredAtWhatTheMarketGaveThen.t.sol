// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// The account can't tell the leaver's fill from the agent's own sale: a
/// position that shrank after her order missed counts as her close. So the
/// least her close fetched prices each market at the worse of the stage's
/// limit and the limit at the prices now: a fall after a missed order is
/// measured at what the market gave, never at the stage's price, and the
/// stayers write her no put on it.
contract ASaleAfterAMissedOrderIsMeasuredAtWhatTheMarketGaveThenTest is HyperliquidVault {
    function _run(bool missed) internal returns (uint256 paid, uint256 bobLoss, uint256 part) {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        value(vault);
        uint256 bobBefore = _worthOf(bob);
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        uint64 dropped = uint64(uint256(ETH_PX) * 95 / 100);
        if (missed) {
            // The bid her order would have hit is gone when it runs.
            core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX * 985 / 1000, ETH_PX + 10);
            _nextBlock();
            // ETH falls 5%; the agent sells about what her order would
            // have (a little more), at the market.
            _spread(dropped, 10);
            core.trade(address(station), ETH, -340_000, dropped);
        } else {
            // Her order fills at the bid, then ETH falls 5%.
            _nextBlock();
            _spread(dropped, 10);
        }
        skip(hl.DELIVERY_WINDOW());
        station.settleClose();
        part = station.stage().part;
        _nextBlock();
        station.settleClose();
        require(
            vault.claimAt(1).state == ClaimState.Paid || vault.claimAt(1).state == ClaimState.Closing, "moved"
        );
        paid = usd.balanceOf(alice);
        skip(1 hours);
        value(vault);
        uint256 bobAfter = _worthOf(bob);
        bobLoss = bobBefore > bobAfter ? bobBefore - bobAfter : 0;
    }

    function test_a_missed_order_then_the_agents_sale_after_a_fall_is_measured_at_the_fall() public {
        uint256 start = vm.snapshotState();
        (uint256 filled, uint256 bobFilled,) = _run(false);
        vm.revertToState(start);
        (uint256 missed, uint256 bobMissed, uint256 part) = _run(true);
        emit log_named_decimal_uint("paid, her order filled, then ETH -5%", filled, 6);
        emit log_named_decimal_uint("paid, her order missed, ETH -5%, the agent sold", missed, 6);
        emit log_named_decimal_uint("part proved (missed)", part, 18);
        emit log_named_decimal_uint("Bob's loss, filled", bobFilled, 6);
        emit log_named_decimal_uint("Bob's loss, missed", bobMissed, 6);
        // Her order sold nothing; the pool's sale was at -5%. She can't be
        // owed more than her part at that price: Bob's loss would be his own
        // share of the fall (about $4,500), as when her order did fill.
        assertLe(
            bobMissed, bobFilled + 100e6, "the stayer paid the leaver the stage's price for the pool's sale"
        );
    }
}
