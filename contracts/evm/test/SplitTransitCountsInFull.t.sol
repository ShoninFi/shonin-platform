// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {StationPost, Valuation, ValuationStatus, VaultState} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// Money in transit counts at par however it is split: many sends of a
/// dollar each are counted, hold a halted vault's close off and, late, void
/// a valuation, exactly as one large send would.
contract SplitTransitCountsInFullTest is TwoChainVault {
    uint256 internal constant PIECE = MINIMUM_DEPOSIT / 10; // $1: "dust"

    function _splitSend(uint256 pieces) internal {
        vm.startPrank(executor);
        station.pull(pieces * PIECE);
        for (uint256 i = 0; i < pieces; i++) {
            station.bridge("local-bridge", OTHER, PIECE, "");
        }
        vm.stopPrank();
    }

    function _honestPostNamingAll(uint256 sends) internal pure returns (StationPost memory p) {
        p = postOf(0, 0);
        p.inFlightFromHome = new uint64[](sends);
        for (uint256 i = 0; i < sends; i++) {
            p.inFlightFromHome[i] = uint64(i);
        }
    }

    /// A deposit priced while the pool's money is on its way in many small
    /// sends buys at the pool's whole worth.
    function test_a_deposit_beside_split_sends_buys_at_the_pools_whole_worth() public {
        uint256 aliceShares = depositTwo(alice, 1000e6);

        // The strategy moves $900 to the other chain as 900 one-dollar sends.
        _splitSend(900);

        // Bob asks to deposit $1000.
        requestDepositAs(vault, bob, 1000e6);

        // The executor's honest post: nothing landed yet, all 900 sends named.
        uint64 time = open();
        post(time, _honestPostNamingAll(900), EXECUTOR_KEY);
        closeAfterDeadline(time);
        Valuation memory v = latest();
        assertTrue(v.status == ValuationStatus.Priced);
        // The $900 on its way is counted at par.
        assertEq(v.gross, v.cash + 900e6 + 1, "money in transit was not counted");
        uint256 bobShares = vault.balanceOf(bob);

        // The money lands and the strategy brings it home.
        give(address(station), 900e6);
        vm.prank(executor);
        station.push(900e6);

        // Bob leaves at the next valuation.
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        valueWith(0, 0);
        uint256 bobBack = usd.balanceOf(bob);

        // Invariant 3: a deposit followed by a withdrawal never returns more
        // than was put in.
        assertLe(bobBack, 1000e6, "deposit then withdraw returned far more than was put in");

        // And alice, who never moved, kept her money.
        Valuation memory w = latest();
        uint256 aliceWorth = aliceShares * (w.gross - w.fee) / w.supply;
        assertGe(aliceWorth, 990e6, "the stayer paid for bob's move");
    }

    /// A halted vault whose money is on its way in many small sends doesn't
    /// close until it has landed.
    function test_a_halted_vault_does_not_close_with_split_sends_on_the_way() public {
        uint256 aliceShares = depositTwo(alice, 1000e6);
        uint256 bobShares = depositTwo(bob, 1000e6);
        vm.prank(executor);
        station.pull(1800e6);

        vm.prank(admin);
        vault.kill();
        // Inside the halt grace the executor still holds the home station.
        vm.startPrank(executor);
        for (uint256 i = 0; i < 1800; i++) {
            station.bridge("local-bridge", OTHER, PIECE, "");
        }
        vm.stopPrank();

        uint64 time = open();
        post(time, _honestPostNamingAll(1800), EXECUTOR_KEY);
        closeAfterDeadline(time);
        // Invariant: a vault is closed only when the stations hold nothing
        // and nothing is on its way.
        bool closed;
        try vault.close() {
            closed = true;
        } catch {}
        if (closed) {
            assertTrue(vault.state() == VaultState.Closed);
            vm.prank(alice);
            uint256 alicePaid = vault.redeemClosed(aliceShares, alice);

            // The $1800 lands back home; anyone stops and unwinds the station.
            give(address(station), 1800e6);
            skip(HALT_GRACE);
            station.stop();
            station.unwind(1e18);
            vm.prank(bob);
            uint256 bobPaid = vault.redeemClosed(bobShares, bob);
            assertApproxEqRel(alicePaid, bobPaid, 0.01e18, "two equal holders were paid very differently");
        }
        assertFalse(closed, "a halted vault closed with $1800 of its money on the way");
    }
}
