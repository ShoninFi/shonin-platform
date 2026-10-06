// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ValuationStatus, VaultState} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// A stopped station's unwind is open to anyone, and through a venue that
/// acts after the transaction each one that moves dollars voids valuations
/// for a while. A halted vault closes only at a priced valuation, so nobody
/// repeating the unwind may keep it from ever being priced: an unwind
/// through an account worth no more than a dollar moves nothing that could
/// go uncounted and is not a move, and one that is a move is refused sooner
/// than the driver spaces the executor's moves, so a priced slot always
/// fits between two of them.
contract NobodysUnwindKeepsAStoppedHyperliquidStationUnpricedTest is HyperliquidVault {
    uint256 internal priced;
    uint256 internal pricedSince;
    uint256 internal unpricedGaps;
    uint256 internal moves;

    function _status() internal view returns (ValuationStatus) {
        return vault.valuationAt(vault.valuationCount() - 1).status;
    }

    /// Killed and stopped; the keeper's unwinds, each as soon as the driver
    /// allows, bring everything home.
    function _stoppedAndBroughtHome() internal {
        _fundAndTrade();
        _stockGas();
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        for (uint256 i = 0; i < 4; i++) {
            uint256 at = hl.movableAt(address(station));
            if (vm.getBlockTimestamp() < at) vm.warp(at);
            station.unwind(1e18);
            _nextBlock();
        }
        assertEq(core.szi(address(station), ETH), 0, "no position");
        assertGt(vault.cash(), 185_000e6, "the account's worth is home");
    }

    /// For an hour a stranger gives the account `gift` (hundred-millionths
    /// of a dollar) and repeats the unwind every five seconds; the keeper
    /// values every slot.
    function _stranger(uint64 gift) internal {
        uint64 moved = station.movedAt(HL);
        for (uint256 second = 5; second <= 1 hours; second += 5) {
            core.creditSpot(address(station), 0, gift);
            vm.prank(address(0xBAD));
            station.unwind(1e18);
            if (station.movedAt(HL) != moved) {
                moved = station.movedAt(HL);
                // The stretch since the stranger's last move (the first is
                // measured from the keeper's, partly before this began).
                if (moves != 0 && pricedSince == 0) unpricedGaps++;
                moves++;
                pricedSince = 0;
            }
            _nextBlock();
            skip(4);
            if (vm.getBlockTimestamp() % SLOT < 5) {
                value(vault);
                if (_status() == ValuationStatus.Priced) {
                    priced++;
                    pricedSince++;
                }
            }
        }
    }

    function test_a_millionth_of_a_dollar_every_five_seconds_neither_moves_nor_keeps_a_halted_vault_from_closing(
    ) public {
        _stoppedAndBroughtHome();
        _stranger(100);
        assertEq(moves, 0, "an unwind through an account worth under a dollar counted as a move");
        assertGt(priced, 300, "the halted vault was kept unpriced");
        value(vault);
        vault.close();
        assertEq(uint256(vault.state()), uint256(VaultState.Closed), "the halted vault closes");
    }

    function test_a_stranger_paying_dollars_for_every_unwind_still_leaves_a_priced_valuation_between_them()
        public
    {
        _stoppedAndBroughtHome();
        _stranger(2e8);
        assertGt(moves, 20, "the stranger's unwinds moved dollars as often as allowed");
        assertEq(unpricedGaps, 0, "two moves had no priced valuation between them");
    }
}
