// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Stage} from "../../src/interfaces/IStation.sol";
import {ClaimState} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// From the stage until its proof the attested executor trades nothing at
/// the venue, so the account changes only by her orders and by what was set
/// going before the stage (a resting order that fills before the executor
/// cancels it, funding). Her least is her part at her orders' limits from
/// the stage's prices alone, less a fill's fee: a fall after her fill
/// neither lowers it nor reaches her payout, and a loss the account takes
/// after the stage costs her no more than the band and fee on her orders,
/// whatever the price at the proof.
contract AFallAfterHerFillDoesNotChargeTheLeaverTheAccountsLossTest is HyperliquidVault {
    function _measured(function() internal between) internal returns (Stage memory) {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        // Her orders fill at the stage.
        _nextBlock();
        between();
        skip(hl.SETTLE_DELAY());
        station.settleClose();
        return station.stage();
    }

    function _paid(function() internal between) internal returns (uint256) {
        _measured(between);
        _nextBlock();
        station.settleClose();
        require(vault.claimAt(1).state == ClaimState.Paid, "paid");
        return usd.balanceOf(alice);
    }

    function _nothing() internal {}

    function _aFall() internal {
        _spread(ETH_PX * 95 / 100, 10);
    }

    /// A round trip that loses $5,400 and leaves the position as it was (an
    /// order resting before the stage, filled before it was cancelled, and
    /// its unwind), then ETH falls 5% before the proof.
    function _aLossThenAFall() internal {
        core.trade(address(station), ETH, 1_000_000, ETH_PX * 101 / 100);
        core.trade(address(station), ETH, -1_000_000, ETH_PX * 99 / 100);
        _spread(ETH_PX * 95 / 100, 10);
    }

    function test_a_fall_after_her_fill_does_not_change_what_she_is_paid() public {
        uint256 start = vm.snapshotState();
        uint256 flat = _paid(_nothing);
        vm.revertToState(start);
        uint256 fell = _paid(_aFall);
        emit log_named_decimal_uint("paid, price unchanged at the proof", flat, 6);
        emit log_named_decimal_uint("paid, ETH -5% at the proof", fell, 6);
        assertApproxEqAbs(fell, flat, 5e6, "a fall after her fill changed what she was paid");
    }

    function test_a_fall_after_her_fill_does_not_charge_the_leaver_the_accounts_loss() public {
        uint256 start = vm.snapshotState();
        Stage memory plain = _measured(_aFall);
        vm.revertToState(start);
        Stage memory lost = _measured(_aLossThenAFall);
        emit log_named_decimal_uint("fetched, ETH -5%", plain.fetched, 6);
        emit log_named_decimal_uint("fetched, the account loses $5,400, then -5%", lost.fetched, 6);
        assertEq(lost.part, 1e18, "her whole close");
        // The band and a fill's fee on her 33.3 ETH at the stage's price
        // are about $990: none of the fall, and no more of the loss.
        assertGe(
            lost.fetched + 1000e6,
            plain.fetched,
            "the leaver paid the account's loss beyond her orders' limits"
        );
    }
}
