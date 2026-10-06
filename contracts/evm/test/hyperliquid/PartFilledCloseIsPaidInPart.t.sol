// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// A staged close that the venue fills only in part still ends: once its
/// window is over it is measured for the part that closed, the leaver is
/// paid for that part of their claim and bears every dollar of slippage of
/// everything their stage sold, and the claim waits with the rest of its
/// shares. What it sold is never left for the holders who stay to pay, so a
/// leaver with a friend on a thin book can't take the band from them at
/// every try, and a strategy trading over its own close doesn't make the
/// stayers pay either.
contract PartFilledCloseIsPaidInPartTest is HyperliquidVault {
    function test_a_part_filled_close_pays_its_part_and_costs_the_stayers_nothing() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        value(vault);
        uint256 bobBefore = _worthOf(bob);

        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        assertEq(core.queued(), 1, "her half of the long, 33.3334 ETH, is ordered sold");

        // Before the venue acts the bid sits 0.9% under the oracle, inside
        // the band (1%), with 30 ETH behind it: the order fills 30 of its
        // 33.3334 ETH there, $729 under the mark, and the rest is cancelled.
        core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX * 991 / 1000, ETH_PX + 10);
        core.setDepth(300_000);
        _nextBlock();
        core.setDepth(0);
        _spread(ETH_PX, 10);
        assertEq(core.szi(address(station), ETH), 366_667, "30 ETH were sold for her claim");

        // Once its window has passed it is measured for the 90% that closed,
        // and delivered.
        skip(station.stage().window);
        station.settleClose();
        _nextBlock();
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "the stage has ended");
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "the claim waits with what is left");
        uint256 left = vault.claimAt(1).shares;
        // 30 ETH of the 33.35 ordered (her fraction is a little over a half).
        assertApproxEqRel(left, aliceShares / 10, 0.005e18, "a tenth of her shares are left");
        assertApproxEqRel(usd.balanceOf(alice), 90_000e6 - 800e6, 0.01e18, "paid for nine tenths");

        value(vault);
        // The fill cost $729: Bob, who asked for nothing, carries none of it.
        // A dollar covers the holding fee and rounding meanwhile.
        assertGe(_worthOf(bob) + 1e6, bobBefore, "the stayer must not pay for the leaver's fill");

        // The rest of her claim is closed at its next turn, and paid.
        vault.settleClaim(1);
        _settleStaged();
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "the rest is paid");
        value(vault);
        assertGe(_worthOf(bob) + 1e6, bobBefore, "and still costs the stayer nothing");
    }

    /// The same without anyone arranging anything: the close fills whole,
    /// and a strategy that quotes both sides has one of its own bids filled
    /// before the proof. The position is then a lot larger than the close
    /// asked for, so the close is a lot short of whole.
    function test_a_strategys_own_fill_before_the_proof_leaves_a_dust_remainder_that_is_paid() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        value(vault);
        uint256 bobBefore = _worthOf(bob);

        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        // Her order fills whole, 0.9% under the oracle: $810 under the mark.
        core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX * 991 / 1000, ETH_PX + 10);
        _nextBlock();
        _spread(ETH_PX, 10);
        // The strategy's own resting bid fills for 0.01 ETH, at the mark.
        core.trade(address(station), ETH, 100, ETH_PX);

        skip(station.stage().window);
        station.settleClose();
        _nextBlock();
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "ended, not lapsed");
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting);
        assertLt(vault.claimAt(1).shares, aliceShares / 1000, "all but a sliver of her claim is paid");

        value(vault);
        assertGe(_worthOf(bob) + 1e6, bobBefore, "the stayer must not pay for the leaver's fill");

        // The sliver is paid at its next turn: from the cash, which covers
        // it, at the next valuation.
        vault.settleClaim(1);
        if (vault.claimAt(1).state == ClaimState.Closing) _settleStaged();
        if (vault.claimAt(1).state == ClaimState.Queued) {
            skip(1 minutes);
            value(vault);
        }
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "the remainder is paid, not left");
        value(vault);
        assertGe(_worthOf(bob) + 1e6, bobBefore, "and costs the stayer nothing");
    }

    /// A close that sold something at a loss and closed nothing whole: the
    /// loss is the leaver's, taken as a slice of their shares, burned, and
    /// nothing is paid; the stayers lose nothing.
    function test_a_fill_of_nothing_whole_at_a_loss_costs_the_leaver_shares_and_the_stayer_nothing() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        value(vault);
        uint256 bobBefore = _worthOf(bob);

        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        // 30 ETH of the order fill 0.9% under the mark; then the strategy
        // buys back the same 30 ETH at the mark: no part of her close is
        // left done, and the $729 is lost.
        core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX * 991 / 1000, ETH_PX + 10);
        core.setDepth(300_000);
        _nextBlock();
        core.setDepth(0);
        _spread(ETH_PX, 10);
        core.trade(address(station), ETH, 300_000, ETH_PX);

        uint256 supplyBefore = vault.totalSupply();
        skip(station.stage().window);
        vm.expectEmit(true, false, false, false, address(vault));
        emit IVault.ClaimPartPaid(1, 0, 0, 0);
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "ended at once: nothing to ask for");
        assertEq(usd.balanceOf(alice), 0, "nothing paid");
        uint256 burned = aliceShares - vault.claimAt(1).shares;
        assertGt(burned, 0, "a slice of her shares is burned for the loss");
        assertEq(supplyBefore - vault.totalSupply(), burned);
        // $729 at about $1 a share, rounded up.
        assertApproxEqRel(burned * (100_000e6 - 1) / aliceShares, 729e6, 0.05e18);
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "the claim waits with the rest");

        value(vault);
        assertGe(_worthOf(bob) + 1e6, bobBefore, "the stayer must not pay for the leaver's loss");
    }
}
