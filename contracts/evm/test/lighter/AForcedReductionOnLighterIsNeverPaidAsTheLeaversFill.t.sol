// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {IStation} from "../../src/interfaces/IStation.sol";
import {ClaimState} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";

/// On Lighter as on Hyperliquid: a post names positions, not whose trades
/// made them, so a reduction the exchange forces on the accounts while a
/// claim's close is staged reads like her fill. A position smaller than
/// her order could have made it, crossed to the other side, or larger,
/// lapses the stage once its window is over; one whose accounts are worth
/// less than her orders could have fetched at their limits lapses at its
/// proof. A lapse pays nobody anything and asks the exchange for nothing:
/// her claim waits whole and what was sold stays the pool's. A reduction
/// inside her order at no worse than her limit is paid as measured.
contract AForcedReductionOnLighterIsNeverPaidAsTheLeaversFillTest is LighterVault {
    struct Before {
        uint256 aliceShares;
        uint256 vaultCash;
        uint256 withdrawals;
        uint256 stationDollars;
        uint256 vaultDollars;
        uint256 executorDollars;
        uint256 bobDollars;
        int256[] snapshot;
    }

    function test_a_forced_reduction_larger_than_her_order_on_lighter_lapses() public {
        Before memory was = _stagedAndMissed();
        // 30 of the 40 ETH at 2,500; she ordered 20.
        exchange.force(_master(), ETH, -300_000, 250_000);
        _lapsesOnceItsWindowIsOver(was);
    }

    function test_a_forced_reduction_inside_her_order_below_her_limit_on_lighter_lapses() public {
        Before memory was = _stagedAndMissed();
        // 10 of her 20 ETH at 2,500, under her 2,673 limit.
        exchange.force(_master(), ETH, -100_000, 250_000);
        skip(lighter.SETTLE_DELAY());
        _post();
        (uint256 part, bool known) = lighter.closedPart(address(station), was.snapshot, _fraction(), 0);
        assertTrue(known, "inside her order: a part is shown");
        assertApproxEqRel(part, 0.5e18, 0.001e18);
        _lapsesOnceItsWindowIsOver(was);
    }

    function test_a_forced_reduction_inside_her_order_at_her_limit_or_better_on_lighter_is_paid_as_measured()
        public
    {
        uint256 aliceShares = _fundAndTrade();
        value(vault);
        uint256 bobBefore = _worth(bob);
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        uint256 fraction = _fraction();
        _missHerOrder();
        // 10 of her 20 ETH at the index, above her limit.
        exchange.force(_master(), ETH, -100_000, ETH_PX);
        uint256 worth = uint256(exchange.equity(_master(), _eth()));

        skip(lighter.DELIVERY_WINDOW());
        _post();
        station.settleClose();
        uint256 part = station.stage().part;
        uint256 fetched = station.stage().fetched;
        assertApproxEqRel(part, 0.5e18, 0.001e18, "the part shown closed");
        uint256 hers = worth * (fraction * part / 1e18) / 1e18;
        assertLe(fetched, hers, "no more than her part of what the accounts are worth");
        assertApproxEqRel(fetched, hers, 0.001e18);
        exchange.process();
        _collect();
        _nextBlock();
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "delivered");
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "the rest of her claim waits");
        assertGt(usd.balanceOf(alice), 0, "paid for the part");

        skip(lighter.SETTLE_DELAY());
        _post();
        value(vault);
        assertGe(_worth(bob) + 1e6, bobBefore, "the stayer must not pay for it");
    }

    function test_a_position_crossed_to_the_other_side_on_lighter_lapses() public {
        Before memory was = _stagedAndMissed();
        exchange.force(_master(), ETH, -500_000, ETH_PX);
        assertLt(exchange.size(_master(), ETH), 0, "short now");
        _lapsesOnceItsWindowIsOver(was);
    }

    function test_a_position_that_grew_on_lighter_lapses() public {
        Before memory was = _stagedAndMissed();
        exchange.force(_master(), ETH, 10_000, ETH_PX);
        _lapsesOnceItsWindowIsOver(was);
    }

    // Helpers ----------------------------------------------------------------

    function _eth() internal pure returns (uint16[] memory markets) {
        markets = new uint16[](1);
        markets[0] = ETH;
    }

    function _worth(address who) internal view returns (uint256) {
        uint256 count = vault.valuationCount();
        return vault.balanceOf(who) * (vault.valuationAt(count - 1).gross - 1)
            / vault.valuationAt(count - 1).supply;
    }

    function _fraction() internal view returns (uint256) {
        return station.stage().fraction;
    }

    /// The exchange runs her order with the mark past her limit: nothing
    /// of it fills. The mark is then back where it was.
    function _missHerOrder() internal {
        _nextBlock();
        exchange.setMark(ETH, uint32(uint256(ETH_PX) * 985 / 1000));
        exchange.process();
        exchange.setMark(ETH, ETH_PX);
        require(exchange.size(_master(), ETH) == 400_000, "her order missed");
    }

    function _stagedAndMissed() internal returns (Before memory was) {
        uint256 aliceShares = _fundAndTrade();
        _aliceClaims(aliceShares);
        was.vaultCash = vault.cash();
        vault.settleClaim(1);
        require(vault.claimAt(1).state == ClaimState.Closing, "staged");
        was.aliceShares = aliceShares;
        // What the stage recorded: the post it was sized from.
        was.snapshot = lighter.snapshot(address(station));
        _missHerOrder();
        was.withdrawals = exchange.withdrawalCount();
        was.stationDollars = usd.balanceOf(address(station));
        was.vaultDollars = usd.balanceOf(address(vault));
        was.executorDollars = usd.balanceOf(executor);
        was.bobDollars = usd.balanceOf(bob);
    }

    function _lapsesOnceItsWindowIsOver(Before memory was) internal {
        int256 cash = exchange.cash(_master());
        uint256 fraction = _fraction();
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
        assertEq(usd.balanceOf(alice), 0, "nothing paid to her");
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "her claim waits");
        assertEq(vault.claimAt(1).shares, was.aliceShares, "with all its shares");
        assertEq(vault.cash(), was.vaultCash, "its part of the cash is back");
        assertEq(exchange.withdrawalCount(), was.withdrawals, "nothing asked of the exchange");
        assertEq(exchange.cash(_master()), cash, "the accounts' cash stays the pool's");
        assertEq(usd.balanceOf(address(station)), was.stationDollars, "nothing moved at the station");
        assertEq(usd.balanceOf(address(vault)), was.vaultDollars, "nor at the vault");
        assertEq(usd.balanceOf(executor), was.executorDollars, "nothing to the executor");
        assertEq(usd.balanceOf(bob), was.bobDollars, "nothing to the stayer");
    }
}
