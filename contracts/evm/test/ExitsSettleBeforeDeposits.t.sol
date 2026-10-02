// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Valuation} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Inside one slot, exits settle before deposits whatever order they were
/// asked in: each exit meets the pool exactly as the snapshot found it, so
/// a deposit that joins above the mark and raises it costs no stayer
/// anything when an exit follows it in the queue.
contract ExitsSettleBeforeDepositsTest is ClaimBase {
    uint256 internal constant PRICE_SCALE = 1e30;

    /// What a share is worth now, after the profit fee the vault owes now:
    /// the same formula a valuation uses, on the live state.
    function liveNetPrice() internal view returns (uint256) {
        (uint256 stationMark,) = station.value();
        uint256 gross = vault.cash() + stationMark + 1;
        uint256 supply = vault.totalSupply() + 1e12;
        uint256 basis = (vault.mark() * supply + PRICE_SCALE - 1) / PRICE_SCALE;
        uint256 fee = gross > basis ? (gross - basis) * 1500 / 10_000 : 0;
        return (gross - fee) * 1e18 / supply;
    }

    function snapshotNetPrice() internal view returns (uint256) {
        Valuation memory v = latest();
        return (v.gross - v.fee) * 1e18 / v.supply;
    }

    /// Alice in, most of it in BTC, and BTC up 40%: the vault is well above
    /// its mark, with a profit fee owed and not yet paid.
    function inProfit() internal returns (uint256 aliceShares) {
        aliceShares = depositAs(vault, alice, 100_000e6);
        deploySpot(90_000e6, 90_000e6, 1.5e18);
        setPrice(SPOT, 84_000e6, 0);
        value(vault);
        assertGt(latest().fee, 0, "no profit fee owed");
    }

    function test_an_exit_asked_after_a_mark_raising_deposit_costs_the_stayers_nothing() public {
        uint256 aliceShares = inProfit();

        // In one slot: bob deposits first, then alice redeems.
        requestDepositAs(vault, bob, 300_000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        uint256 markBefore = vault.mark();
        value(vault);
        assertGt(vault.mark(), markBefore, "the deposit didn't raise the mark");
        assertEq(vault.balanceOf(alice), 0, "alice wasn't paid");

        uint256 snapshot = snapshotNetPrice();
        uint256 live = liveNetPrice();
        emit log_named_uint("snapshot net price (1e18 = $1)", snapshot);
        emit log_named_uint("live net price after the slot  ", live);
        // Bob (and every other remaining share) should be where the slot
        // priced him; the pool's transaction fees can only push it up.
        assertGe(live, snapshot, "the stayers paid for alice's exit");
    }

    /// The same two requests the other way round: nobody's price moves.
    function test_an_exit_asked_first_moves_nobody() public {
        uint256 aliceShares = inProfit();
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        requestDepositAs(vault, bob, 300_000e6);
        value(vault);
        assertGe(liveNetPrice(), snapshotNetPrice());
    }
}
