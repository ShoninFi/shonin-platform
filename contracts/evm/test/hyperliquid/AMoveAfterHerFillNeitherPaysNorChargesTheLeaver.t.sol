// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// The least a staged close fetched is her part of the account with every
/// order filled at its limit from the stage's own prices: fixed when the
/// orders were placed, so a move after her fill neither pays her more (an
/// option on the price the stayers would write) nor charges her less.
contract AMoveAfterHerFillNeitherPaysNorChargesTheLeaverTest is HyperliquidVault {
    function _payout(uint64 pxAtProof) internal returns (uint256 paid, uint256 bobLoss) {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        value(vault);
        uint256 bobBefore = _worthOf(bob);
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        // Her order fills 0.9% under the oracle, inside the band (the
        // leaver can arrange that herself, with a bid of her own there).
        core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX * 991 / 1000, ETH_PX + 10);
        _nextBlock();
        // The market moves before the proof.
        _spread(pxAtProof, 10);
        skip(hl.SETTLE_DELAY());
        station.settleClose();
        _nextBlock();
        station.settleClose();
        require(vault.claimAt(1).state == ClaimState.Paid, "paid");
        paid = usd.balanceOf(alice);
        // Back to the stage's price, so Bob's worth compares like for like.
        _spread(ETH_PX, 10);
        skip(1 hours);
        value(vault);
        uint256 bobAfter = _worthOf(bob);
        bobLoss = bobBefore > bobAfter ? bobBefore - bobAfter : 0;
    }

    function test_a_move_after_her_fill_does_not_change_what_she_is_paid() public {
        uint256 start = vm.snapshotState();
        (uint256 flat, uint256 bobFlat) = _payout(ETH_PX);
        vm.revertToState(start);
        (uint256 up, uint256 bobUp) = _payout(uint64(uint256(ETH_PX) * 1005 / 1000));
        emit log_named_decimal_uint("paid, price unchanged at the proof", flat, 6);
        emit log_named_decimal_uint("paid, ETH +0.5% at the proof", up, 6);
        emit log_named_decimal_uint("Bob's loss, unchanged", bobFlat, 6);
        emit log_named_decimal_uint("Bob's loss, +0.5%", bobUp, 6);
        // Her orders were done at the stage: what they fetched is the same.
        assertApproxEqAbs(up, flat, 5e6, "a move after her fill changed what she was paid");
        assertLe(bobUp, bobFlat + 5e6, "the stayer paid the leaver's fill");
    }

    /// Up 3% or down 3% at the proof: she is paid what her fill fetched
    /// either way. Nothing but her orders trades the account until the
    /// proof, and her least reads the stage's prices alone.
    function test_a_move_either_way_after_her_fill_pays_her_neither_more_nor_less() public {
        uint256 start = vm.snapshotState();
        (uint256 flat,) = _payout(ETH_PX);
        vm.revertToState(start);
        (uint256 up,) = _payout(uint64(uint256(ETH_PX) * 103 / 100));
        vm.revertToState(start);
        (uint256 down,) = _payout(uint64(uint256(ETH_PX) * 97 / 100));
        emit log_named_decimal_uint("paid, unchanged", flat, 6);
        emit log_named_decimal_uint("paid, ETH +3% at the proof", up, 6);
        emit log_named_decimal_uint("paid, ETH -3% at the proof", down, 6);
        assertApproxEqAbs(up, flat, 5e6, "an up-move after her fill paid her more");
        assertApproxEqAbs(down, flat, 5e6, "a fall after her fill paid her less");
    }
}
