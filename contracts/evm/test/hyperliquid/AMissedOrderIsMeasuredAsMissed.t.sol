// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// Her order misses (the bid it would have hit is gone when it runs). From
/// the stage until its proof the attested executor trades nothing at the
/// venue, so nothing else shrinks the position: the close is measured as
/// the miss it was, whatever the price did meanwhile, and the stayers pay
/// nothing for it. Her claim is tried again.
contract AMissedOrderIsMeasuredAsMissedTest is HyperliquidVault {
    function _run(uint64 dip) internal returns (uint256 paid, uint256 bobLoss, uint256 part) {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        value(vault);
        uint256 bobBefore = _worthOf(bob);
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX * 985 / 1000, ETH_PX + 10);
        _nextBlock();
        // ETH dips, then is back by the proof.
        _spread(dip, 10);
        _spread(ETH_PX, 10);
        skip(hl.DELIVERY_WINDOW());
        station.settleClose();
        part = station.stage().part;
        _nextBlock();
        if (station.stage().stagedAt != 0) station.settleClose();
        require(vault.claimAt(1).state != ClaimState.Paid, "nothing closed, nothing paid");
        paid = usd.balanceOf(alice);
        skip(1 hours);
        value(vault);
        uint256 bobAfter = _worthOf(bob);
        bobLoss = bobBefore > bobAfter ? bobBefore - bobAfter : 0;
    }

    function test_a_missed_order_is_measured_as_missed() public {
        (uint256 paid, uint256 bobLoss, uint256 part) = _run(uint64(uint256(ETH_PX) * 95 / 100));
        emit log_named_decimal_uint("paid", paid, 6);
        emit log_named_decimal_uint("part proved", part, 18);
        emit log_named_decimal_uint("Bob's loss", bobLoss, 6);
        assertEq(part, 0, "nothing she ordered closed");
        assertEq(paid, 0, "nothing paid for it");
        assertLe(bobLoss, 1e6, "the stayer paid for a close that never happened");
    }
}
