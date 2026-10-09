// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../../src/interfaces/IVault.sol";
import {Valuation} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";

/// On Lighter as on Hyperliquid: her order misses, and from the stage until
/// its proof the attested executor trades nothing at the exchange, so
/// nothing else shrinks the position. The close is measured as the miss it
/// was, whatever the index did meanwhile, and the stayers pay nothing for
/// it.
contract AMissedOrderOnLighterIsMeasuredAsMissedTest is LighterVault {
    function _worth(address who) internal view returns (uint256) {
        Valuation memory v = vault.valuationAt(vault.valuationCount() - 1);
        return vault.balanceOf(who) * (v.gross - 1) / v.supply;
    }

    function test_a_missed_order_on_lighter_is_measured_as_missed() public {
        uint256 aliceShares = _fundAndTrade();
        value(vault);
        uint256 bobBefore = _worth(bob);
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        require(vault.claimAt(1).state == ClaimState.Closing, "staged");
        _nextBlock();
        // The mark is past her limit when the exchange runs her order.
        exchange.setMark(ETH, uint32(uint256(ETH_PX) * 985 / 1000));
        exchange.process();
        // ETH dips 5%, then is back by the post the proof reads.
        exchange.setMark(ETH, uint32(uint256(ETH_PX) * 95 / 100));
        exchange.setMark(ETH, ETH_PX);
        skip(lighter.DELIVERY_WINDOW());
        _post();
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "measured as nothing, and ended");
        assertTrue(vault.claimAt(1).state != ClaimState.Paid, "nothing closed, nothing paid");
        assertEq(usd.balanceOf(alice), 0, "nothing paid for it");
        skip(lighter.SETTLE_DELAY());
        _post();
        value(vault);
        uint256 bobAfter = _worth(bob);
        uint256 bobLoss = bobBefore > bobAfter ? bobBefore - bobAfter : 0;
        emit log_named_decimal_uint("Bob's loss", bobLoss, 6);
        assertLe(bobLoss, 1e6, "the stayer paid for a close that never happened");
    }
}
