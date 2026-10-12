// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {IStation} from "../../src/interfaces/IStation.sol";
import {ClaimState, VaultState} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// The venue's reads name positions, not whose trades made them: a reduction
/// the venue forces on the account (a liquidation, a deleveraging) while a
/// claim's close is staged reads just like her fill. So a stage is paid only
/// on evidence that can all be hers. A position smaller than her order could
/// have made it, crossed to the other side, or larger, can't be told apart,
/// and the stage lapses once its window is over; one whose account is worth
/// less than her orders could have fetched at their limits lapses at its
/// proof. A lapse pays nobody anything: the leaver keeps her claim, which a
/// later stage closes from the account as it is then, and whatever was sold
/// stays the pool's. A reduction inside her order at no worse than her limit
/// is paid as measured, never more than the account's change.
contract AForcedReductionIsNeverPaidAsTheLeaversFillTest is HyperliquidVault {
    /// What a lapse must leave as it was.
    struct Before {
        uint256 aliceShares;
        uint256 vaultCash;
        int256 ledger;
        uint256 stationDollars;
        uint256 vaultDollars;
        uint256 executorDollars;
        uint256 bobDollars;
        int256[] snapshot;
    }

    // The account-state stand-ins ------------------------------------------

    /// 20,000 of collateral, 100 ETH long at 1,000, half of it ordered
    /// closed; the order fills nothing and the venue closes all 100 at 850,
    /// leaving 5,000. Nothing of that is hers to be paid.
    function test_a_forced_close_of_more_than_her_order_proves_nothing() public {
        _spread(100_000, 10);
        core.credit(address(station), 20_000e6);
        core.trade(address(station), ETH, 1_000_000, 100_000);
        int256[] memory before = hl.snapshot(address(station));
        int256 ledger = hl.ledger(address(station));
        _spread(85_000, 10);
        core.trade(address(station), ETH, -1_000_000, 85_000);
        assertEq(hl.ledger(address(station)), 5000e6);

        (uint256 part, bool known) = hl.closedPart(address(station), before, 0.5e18, uint64(block.timestamp));
        assertFalse(known, "a position smaller than her order could make it is not hers to prove");
        assertEq(part, 0);
        // Counted as hers whole, the measure would sit far below what her
        // orders could have fetched at their limits: the station lapses
        // that too, rather than pay the difference out of the pool.
        (int256 measured, int256 least) = hl.measure(address(station), ledger, before, 0.5e18, 1e18, 100);
        assertEq(measured, 2500e6);
        assertEq(least, 9450e6);
        assertLt(measured, least);
    }

    /// The venue closes 75 of 100 ETH at 850 and then her own order closes
    /// 25 at 990, leaving 8,500. Even given the part that was truly hers
    /// (half of the 50 ordered), the account lost more than her orders
    /// could have: the stage lapses either way, and nobody takes the
    /// pool's loss out of it.
    function test_a_forced_reduction_before_her_own_fill_proves_nothing() public {
        _spread(100_000, 10);
        core.credit(address(station), 20_000e6);
        core.trade(address(station), ETH, 1_000_000, 100_000);
        int256[] memory before = hl.snapshot(address(station));
        int256 ledger = hl.ledger(address(station));
        _spread(85_000, 10);
        core.trade(address(station), ETH, -750_000, 85_000);
        _spread(99_000, 10);
        core.trade(address(station), ETH, -250_000, 99_000);
        assertEq(hl.ledger(address(station)), 8500e6);

        (, bool known) = hl.closedPart(address(station), before, 0.5e18, uint64(block.timestamp));
        assertFalse(known, "smaller by more than her order: not hers");
        (int256 measured, int256 least) = hl.measure(address(station), ledger, before, 0.5e18, 0.5e18, 100);
        assertEq(measured, -5750e6);
        assertEq(least, 4475e6);
        assertLt(measured, least, "below what her orders could fetch: the station lapses it");
    }

    // Through the vault ------------------------------------------------------

    /// A vault whose account holds 1,000 ETH at 1,000 on 190,000; Alice's
    /// claim (no more than 51% of the units) is staged, her order misses,
    /// and the venue closes the whole position at 950. The stage lapses
    /// with nothing paid: the 140,000 left stays at the venue, the pool's.
    function test_a_forced_close_of_everything_after_her_order_missed_lapses_and_the_pool_keeps_the_cash()
        public
    {
        uint256 shares = _fundAndTrade();
        core.trade(address(station), ETH, -666_667, ETH_PX);
        _spread(100_000, 10);
        core.trade(address(station), ETH, 10_000_000, 100_000);
        _stockGas();
        assertLe(shares * 100, vault.totalSupply() * 51);
        _aliceClaims(shares);
        uint256 vaultCash = vault.cash();
        vault.settleClaim(1);
        require(vault.claimAt(1).state == ClaimState.Closing, "staged");
        uint256 fraction = station.stage().fraction;

        // Her order misses; then the venue closes everything at 950.
        core.setPerp(ETH, 100_000, 100_000, 90_000, 110_000);
        _nextBlock();
        assertEq(core.szi(address(station), ETH), 10_000_000, "her order missed");
        _spread(100_000, 10);
        core.trade(address(station), ETH, -10_000_000, 95_000);
        assertEq(hl.ledger(address(station)), 140_000e6);
        Before memory was = _before(shares, vaultCash);

        skip(hl.SETTLE_DELAY());
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
        skip(station.stage().window);
        vm.expectEmit(address(station));
        emit IStation.CloseLapsed(fraction);
        station.settleClose();
        _assertLapsed(was);
        assertEq(hl.ledger(address(station)), 140_000e6, "the venue's cash stays the pool's");
    }

    /// The venue closes more than her order, at a loss, after her order
    /// missed: the stage lapses with nothing paid.
    function test_a_forced_reduction_larger_than_her_order_lapses() public {
        Before memory was = _stagedAndMissed();
        // 50 of the 66.67 ETH at 2,500; she ordered 33.33.
        core.trade(address(station), ETH, -500_000, 250_000);
        _lapsesOnceItsWindowIsOver(was);
    }

    /// Inside her order, at a price below her limit: what she is shown to
    /// have closed fetched less than her orders could have, the account
    /// lost what she couldn't, and the stage lapses at its proof.
    function test_a_forced_reduction_inside_her_order_below_her_limit_lapses() public {
        Before memory was = _stagedAndMissed();
        // 20 of her 33.33 ETH at 2,500, under her 2,673 limit.
        core.trade(address(station), ETH, -200_000, 250_000);
        (uint256 part, bool known) = hl.closedPart(address(station), was.snapshot, _fraction(), 0);
        assertTrue(known, "inside her order: a part is shown");
        assertApproxEqRel(part, 0.6e18, 0.001e18);
        _lapsesOnceItsWindowIsOver(was);
    }

    /// Inside her order at no worse than her limit: it is paid as measured,
    /// her part of what the account then is, and the stayer loses nothing.
    function test_a_forced_reduction_inside_her_order_at_her_limit_or_better_is_paid_as_measured() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        value(vault);
        uint256 bobBefore = _worthOf(bob);
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        uint256 fraction = station.stage().fraction;
        core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX * 98 / 100, ETH_PX + 10);
        _nextBlock();
        require(core.szi(address(station), ETH) == 666_667, "her order missed");
        _spread(ETH_PX, 10);
        // 20 of her 33.33 ETH at the mark, above her limit.
        core.trade(address(station), ETH, -200_000, ETH_PX);
        uint256 worth = uint256(
            hl.ledger(address(station)) + int256(core.szi(address(station), ETH)) * int256(uint256(ETH_PX))
        );

        skip(station.stage().window);
        station.settleClose();
        uint256 part = station.stage().part;
        uint256 fetched = station.stage().fetched;
        assertApproxEqRel(part, 0.6e18, 0.001e18, "the part shown closed");
        // Her part of what the account is worth now (unchanged by a sale
        // at the mark), never more.
        uint256 hers = worth * (fraction * part / 1e18) / 1e18;
        assertLe(fetched, hers, "no more than her part of it");
        assertApproxEqRel(fetched, hers, 0.001e18);
        _nextBlock();
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "delivered");
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "the rest of her claim waits");
        assertGt(usd.balanceOf(alice), 0, "paid for the part");

        value(vault);
        assertGe(_worthOf(bob) + 1e6, bobBefore, "the stayer must not pay for it");
    }

    /// Crossed to the other side: more than closed, by something else.
    function test_a_position_crossed_to_the_other_side_lapses() public {
        Before memory was = _stagedAndMissed();
        core.trade(address(station), ETH, -1_000_000, ETH_PX);
        assertLt(core.szi(address(station), ETH), 0, "short now");
        _lapsesOnceItsWindowIsOver(was);
    }

    /// Larger than at the stage: something else traded the account.
    function test_a_position_that_grew_lapses() public {
        Before memory was = _stagedAndMissed();
        core.trade(address(station), ETH, 100_000, ETH_PX);
        _lapsesOnceItsWindowIsOver(was);
    }

    /// Her order fills whole, and the venue then takes 10 ETH more: the
    /// position is smaller than her order could make it, and the stage
    /// lapses, her fill included: it stays the pool's.
    function test_her_whole_fill_with_a_forced_reduction_beside_it_lapses() public {
        Before memory was = _staged();
        _nextBlock();
        require(core.szi(address(station), ETH) < 340_000, "her order filled");
        core.trade(address(station), ETH, -100_000, ETH_PX * 95 / 100);
        was.ledger = hl.ledger(address(station));
        // Not whole: it waits for its window.
        skip(hl.SETTLE_DELAY());
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
        skip(station.stage().window);
        station.settleClose();
        _assertLapsed(was);
    }

    // After a lapse ----------------------------------------------------------

    /// A lapse leaves her claim to a later stage, which closes it from the
    /// account as the forced reduction left it: she bears her share of the
    /// pool's loss, as everyone does, and no more.
    function test_after_a_lapse_her_claim_is_closed_from_the_account_as_it_is_then() public {
        Before memory was = _stagedAndMissed();
        core.trade(address(station), ETH, -500_000, 250_000);
        _lapsesOnceItsWindowIsOver(was);

        skip(hl.DELIVERY_WINDOW());
        value(vault);
        uint256 pool = uint256(hl.ledger(address(station)))
            + uint256(int256(core.szi(address(station), ETH))) * ETH_PX + vault.cash();
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Closing, "staged again");
        _settleStaged();
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "she is paid");
        // Her units were half the pool's: the 10,000 the venue lost is
        // shared, so she is paid no more than about half of what is left.
        assertLe(usd.balanceOf(alice), pool / 2, "never more than her half of the pool left");
        assertApproxEqRel(usd.balanceOf(alice), pool / 2, 0.01e18);
    }

    /// Stage after stage lapsing never holds the vault: the claim is
    /// counted toward a freeze from its booking, whatever was tried.
    function test_stages_that_keep_lapsing_end_in_the_freeze() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        _aliceClaims(aliceShares);
        uint64 bookedAt = vault.claimAt(1).bookedAt;
        for (uint256 i = 0; i < 3; i++) {
            vault.settleClaim(1);
            require(vault.claimAt(1).state == ClaimState.Closing, "staged");
            // Her order misses, and the account grows by something else.
            core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX * 98 / 100, ETH_PX + 10);
            _nextBlock();
            _spread(ETH_PX, 10);
            core.trade(address(station), ETH, 10, ETH_PX);
            skip(station.stage().window);
            station.settleClose();
            assertEq(station.stage().stagedAt, 0, "lapsed");
            assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "the claim waits");
            skip(hl.DELIVERY_WINDOW());
            value(vault);
        }
        assertEq(usd.balanceOf(alice), 0, "nothing paid on any of them");
        vm.warp(bookedAt + 7 days);
        vault.freeze(1);
        assertTrue(vault.state() == VaultState.Frozen, "frozen: the vault winds down and pays everyone");
    }

    // Helpers ----------------------------------------------------------------

    function _staged() internal returns (Before memory was) {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        _aliceClaims(aliceShares);
        uint256 vaultCash = vault.cash();
        vault.settleClaim(1);
        require(vault.claimAt(1).state == ClaimState.Closing, "staged");
        was = _before(aliceShares, vaultCash);
        // What the stage recorded: nothing has moved since.
        was.snapshot = hl.snapshot(address(station));
    }

    /// Alice's claim staged and her order run by the venue, which finds the
    /// bid it would hit gone: nothing of it fills.
    function _stagedAndMissed() internal returns (Before memory was) {
        was = _staged();
        core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX * 98 / 100, ETH_PX + 10);
        _nextBlock();
        require(core.szi(address(station), ETH) == 666_667, "her order missed");
        _spread(ETH_PX, 10);
    }

    function _before(uint256 aliceShares, uint256 vaultCash) internal view returns (Before memory was) {
        was.aliceShares = aliceShares;
        was.vaultCash = vaultCash;
        was.ledger = hl.ledger(address(station));
        was.stationDollars = usd.balanceOf(address(station));
        was.vaultDollars = usd.balanceOf(address(vault));
        was.executorDollars = usd.balanceOf(executor);
        was.bobDollars = usd.balanceOf(bob);
    }

    function _lapsesOnceItsWindowIsOver(Before memory was) internal {
        was.ledger = hl.ledger(address(station));
        uint256 fraction = _fraction();
        skip(hl.SETTLE_DELAY());
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
        skip(station.stage().window);
        vm.expectEmit(address(station));
        emit IStation.CloseLapsed(fraction);
        station.settleClose();
        _assertLapsed(was);
    }

    /// Nothing paid from the stage to anyone, nothing asked of the venue,
    /// and her claim waiting whole with its cash back in the pool.
    function _assertLapsed(Before memory was) internal view {
        assertEq(station.stage().stagedAt, 0, "the stage has ended");
        assertEq(usd.balanceOf(alice), 0, "nothing paid to her");
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "her claim waits");
        assertEq(vault.claimAt(1).shares, was.aliceShares, "with all its shares");
        assertEq(vault.cash(), was.vaultCash, "its part of the cash is back");
        assertEq(core.queued(), 0, "nothing asked of the venue");
        assertEq(hl.ledger(address(station)), was.ledger, "the venue's cash stays");
        assertEq(usd.balanceOf(address(station)), was.stationDollars, "nothing moved at the station");
        assertEq(usd.balanceOf(address(vault)), was.vaultDollars, "nor at the vault");
        assertEq(usd.balanceOf(executor), was.executorDollars, "nothing to the executor");
        assertEq(usd.balanceOf(bob), was.bobDollars, "nothing to the stayer");
    }

    function _fraction() internal view returns (uint256) {
        return station.stage().fraction;
    }
}
