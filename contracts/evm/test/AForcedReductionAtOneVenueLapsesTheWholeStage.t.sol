// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {IStation} from "../src/interfaces/IStation.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {TwoDelayedVenuesVault} from "./TwoDelayedVenuesVault.sol";

/// A claim's close staged at two venues is proved and measured as one: a
/// position at either venue that something other than her orders changed
/// (smaller than her order could make it, crossed, or larger) leaves the
/// whole stage unproved, and it lapses with nothing paid from either venue,
/// her fills at the other included; they stay the pool's.
contract AForcedReductionAtOneVenueLapsesTheWholeStageTest is TwoDelayedVenuesVault {
    function test_a_close_at_two_venues_that_fills_whole_at_both_is_paid() public {
        uint256 aliceShares = _fundAndTrade();
        value(vault);
        _post();
        uint256 bobBefore = _worthOf(bob);
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        require(vault.claimAt(1).state == ClaimState.Closing, "staged");
        _nextBlock();
        skip(lighter.SETTLE_DELAY());
        _post();
        station.settleClose();
        assertEq(station.stage().part, 1e18, "whole at both");
        _nextBlock();
        _collect();
        station.settleClose();
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "paid");
        assertApproxEqRel(usd.balanceOf(alice), 100_000e6, 0.01e18, "about her half");
        skip(lighter.SETTLE_DELAY());
        _post();
        value(vault);
        assertGe(_worthOf(bob) + 1e6, bobBefore, "the stayer pays nothing for it");
    }

    function test_a_forced_reduction_at_one_venue_lapses_the_stage_at_both() public {
        uint256 aliceShares = _fundAndTrade();
        _aliceClaims(aliceShares);
        uint256 vaultCash = vault.cash();
        vault.settleClaim(1);
        require(vault.claimAt(1).state == ClaimState.Closing, "staged");
        uint256 fraction = station.stage().fraction;
        // Both her orders fill whole.
        _nextBlock();
        assertLt(exchange.size(_master(), LT_ETH), 200_000, "her Lighter order filled");
        assertLt(core.szi(address(station), HL_ETH), 166_667, "her Hyperliquid order filled");
        // Hyperliquid then takes 5 ETH more of the long by itself.
        core.trade(address(station), HL_ETH, -50_000, HL_PX * 95 / 100);
        int256 hlLedger = hl.ledger(address(station));
        int256 lighterCash = exchange.cash(_master());
        uint256 withdrawals = exchange.withdrawalCount();

        skip(lighter.SETTLE_DELAY());
        _post();
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
        skip(lighter.DELIVERY_WINDOW());
        _post();
        vm.expectEmit(address(station));
        emit IStation.CloseLapsed(fraction);
        station.settleClose();

        assertEq(station.stage().stagedAt, 0, "the stage has ended");
        assertEq(usd.balanceOf(alice), 0, "nothing paid, from either venue");
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "her claim waits");
        assertEq(vault.claimAt(1).shares, aliceShares, "whole");
        assertEq(vault.cash(), vaultCash, "its part of the cash is back");
        assertEq(core.queued(), 0, "nothing asked of Hyperliquid");
        assertEq(exchange.withdrawalCount(), withdrawals, "nor of Lighter");
        assertEq(hl.ledger(address(station)), hlLedger, "Hyperliquid's cash stays the pool's");
        assertEq(exchange.cash(_master()), lighterCash, "and Lighter's, her fill there included");
    }

    /// Inside her order at one venue, below her limit, while the other
    /// fills cleanly: the two measures together fall short of what her
    /// orders could have fetched at their limits, and the stage lapses,
    /// the clean fill's proceeds staying the pool's too.
    function test_a_forced_reduction_below_her_limit_at_one_venue_lapses_the_stage_at_both() public {
        uint256 aliceShares = _fundAndTrade();
        _aliceClaims(aliceShares);
        uint256 vaultCash = vault.cash();
        vault.settleClaim(1);
        require(vault.claimAt(1).state == ClaimState.Closing, "staged");
        int256[] memory hlStaged = hl.snapshot(address(station));
        // Her Hyperliquid order misses; her Lighter order fills whole.
        core.setPerp(HL_ETH, HL_PX, HL_PX, HL_PX * 98 / 100, HL_PX + 10);
        _nextBlock();
        _hlSpread(HL_PX);
        assertEq(core.szi(address(station), HL_ETH), 166_667, "her Hyperliquid order missed");
        assertLt(exchange.size(_master(), LT_ETH), 200_000, "her Lighter order filled");
        // Hyperliquid takes 5 of her 8.33 ETH by itself, at 2,500.
        core.trade(address(station), HL_ETH, -50_000, 250_000);
        (uint256 part, bool known) = hl.closedPart(address(station), hlStaged, station.stage().fraction, 0);
        assertTrue(known, "inside her order: a part is shown");
        assertApproxEqRel(part, 0.6e18, 0.001e18);
        int256 hlLedger = hl.ledger(address(station));
        int256 lighterCash = exchange.cash(_master());

        skip(lighter.SETTLE_DELAY());
        _post();
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
        skip(lighter.DELIVERY_WINDOW());
        _post();
        station.settleClose();

        assertEq(station.stage().stagedAt, 0, "the stage has ended");
        assertEq(usd.balanceOf(alice), 0, "nothing paid, from either venue");
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "her claim waits");
        assertEq(vault.claimAt(1).shares, aliceShares, "whole");
        assertEq(vault.cash(), vaultCash, "its part of the cash is back");
        assertEq(core.queued(), 0, "nothing asked of Hyperliquid");
        assertEq(exchange.withdrawalCount(), 0, "nor of Lighter");
        assertEq(hl.ledger(address(station)), hlLedger);
        assertEq(exchange.cash(_master()), lighterCash);
    }
}
