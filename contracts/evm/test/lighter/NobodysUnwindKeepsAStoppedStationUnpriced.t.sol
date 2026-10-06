// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ValuationStatus, VaultState} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";

/// A stopped station's unwind is open to anyone, and each one that asks the
/// exchange for anything voids valuations until what it asked for can be
/// counted. A halted vault closes only at a priced valuation, so nobody
/// repeating the unwind may keep it from ever being priced: an unwind
/// through Lighter is refused sooner than the driver spaces the executor's
/// moves, so a priced slot fits between two of them, and the clock after
/// which a station nobody posts for gives up what it can't count runs from
/// its first unwind at the full band, not its latest.
contract NobodysUnwindKeepsAStoppedLighterStationUnpricedTest is LighterVault {
    function _status() internal view returns (ValuationStatus) {
        return vault.valuationAt(vault.valuationCount() - 1).status;
    }

    /// Killed and stopped; the executor is gone and nobody posts again. The
    /// keeper's unwinds, a week apart, close everything once the widening
    /// band reaches the market, and the pending balance is paid out. The
    /// first unwind at the full band has just run.
    function _stoppedAndClosedAtTheFullBand() internal {
        _fundAndTrade();
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        while (station.fullUnwindAt(LIGHTER) == 0) {
            skip(7 days);
            station.unwind(1e18);
            exchange.process();
            _collect();
        }
        assertGe(station.stoppedBand(), 10_000, "the band is full");
        assertEq(exchange.size(_master(), ETH), 0, "everything is closed");
    }

    function test_an_unwind_sooner_than_the_spacing_after_the_last_move_does_nothing_through_the_venue()
        public
    {
        _fundAndTrade();
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        // The keeper's first unwind places the close and asks for the
        // account's dollars: a move.
        station.unwind(1e18);
        uint64 moved = station.movedAt(LIGHTER);
        assertEq(moved, vm.getBlockTimestamp(), "the unwind moved dollars");
        uint256 asked = exchange.withdrawalCount();
        uint256 ordered = exchange.orderCount();
        // A post read after it; anyone's next unwind, sooner than the
        // driver's spacing: not yet.
        skip(lighter.SETTLE_DELAY());
        _post();
        skip(lighter.POST_MARGIN());
        vm.prank(address(0xBAD));
        (, bool blocked) = station.unwind(1e18);
        assertTrue(blocked, "not yet: reported blocked");
        assertEq(exchange.withdrawalCount(), asked, "nothing more was asked of the exchange");
        assertEq(exchange.orderCount(), ordered, "nothing more was ordered");
        assertEq(station.movedAt(LIGHTER), moved, "the last move did not move");
        // Once a whole slot past it, it goes through.
        skip(SLOT + 3);
        vm.prank(address(0xBAD));
        station.unwind(1e18);
        assertGt(exchange.withdrawalCount(), asked, "asked again once allowed");
    }

    function test_a_stranger_unwinding_as_often_as_allowed_for_two_days_keeps_neither_pricing_nor_the_close_away(
    ) public {
        _stoppedAndClosedAtTheFullBand();
        uint64 first = station.fullUnwindAt(LIGHTER);
        // A stranger tries the unwind every five minutes for two days; the
        // keeper values every ten minutes.
        uint256 priced;
        uint256 valued;
        for (uint256 minute = 5; minute <= 2 days / 1 minutes; minute += 5) {
            skip(5 minutes);
            vm.prank(address(0xBAD));
            station.unwind(1e18);
            if (minute % 10 == 0) {
                value(vault);
                valued++;
                if (_status() == ValuationStatus.Priced) priced++;
            }
        }
        assertEq(station.fullUnwindAt(LIGHTER), first, "the first unwind at the full band stays the clock");
        assertGt(priced, valued / 2, "the stranger's unwinds kept the halted vault unpriced");

        value(vault);
        assertEq(uint256(_status()), uint256(ValuationStatus.Priced), "priced");
        vault.close();
        assertEq(uint256(vault.state()), uint256(VaultState.Closed), "the halted vault closes");
    }
}
