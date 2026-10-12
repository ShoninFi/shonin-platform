// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {LighterReport} from "../../src/drivers/lighter/LighterBoard.sol";
import {ClaimState, Valuation} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";
import {StdStorage, stdStorage} from "forge-std/Test.sol";

/// A staged close on Lighter is proved only by a post read at least the
/// settle delay after the close was placed. A post is dated by when its
/// figures were read, and lands some blocks later, so a post read before
/// the close was placed, landing after it, says nothing about the close,
/// even when the strategy's own sale between the two readings makes it look
/// closed. The close is measured only by a post read after it, and one
/// that shows the position smaller than her order could have made it is
/// never paid as hers.
contract CloseIsProvedOnlyByAPostReadAfterItTest is LighterVault {
    using stdStorage for StdStorage;

    /// The strategy's own key trading on the account: it now holds `lots`.
    function _strategyHolds(int64 lots) internal {
        stdstore.target(address(exchange)).sig("size(uint48,uint16)").with_key(uint256(_master())).with_key(
            uint256(ETH)
        ).checked_write_int(int256(lots));
    }

    function _worthOf(address who) internal view returns (uint256) {
        Valuation memory v = vault.valuationAt(vault.valuationCount() - 1);
        return vault.balanceOf(who) * (v.gross - 1) / v.supply;
    }

    /// Alice's claim is staged on a post showing 40 ETH; between that post
    /// and the next reading the strategy sold 21 ETH itself; the next
    /// reading was made before the stage and lands after it.
    function _stagedBetweenAReadingAndItsPost() internal returns (uint256 bobBefore) {
        uint256 aliceShares = _fundAndTrade();
        value(vault);
        bobBefore = _worthOf(bob);
        _aliceClaims(aliceShares);

        _nextBlock();
        _strategyHolds(190_000);
        LighterReport memory early = _reading();

        _nextBlock();
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Closing);
        assertEq(exchange.orderCount(), 1, "her half of the 40 ETH the stage's post showed is ordered sold");

        vm.prank(executor);
        board.post(address(station), early);
        _nextBlock();
    }

    function test_a_post_read_before_a_close_was_placed_does_not_prove_it() public {
        _stagedBetweenAReadingAndItsPost();
        assertEq(exchange.orderCount(), 1, "the exchange has not carried the order out");
        // The only newer post was read before the order existed: nothing
        // is proved, however long after.
        skip(lighter.SETTLE_DELAY());
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
    }

    /// The early post proves nothing. Once the exchange carries her order
    /// out, the next post shows the position smaller than her order could
    /// have made it: the strategy's own sale before the stage, which the
    /// stage's post didn't show, can't be told from a reduction the exchange
    /// forced, so nothing of it is paid as hers. The stage lapses, and her
    /// claim is closed by a later stage from the accounts as they are then.
    function test_a_claims_close_the_strategys_unseen_sale_makes_look_overfilled_lapses_and_is_closed_again()
        public
    {
        uint256 bobBefore = _stagedBetweenAReadingAndItsPost();
        try station.settleClose() {} catch {}

        // Now the exchange carries her order out, 0.9% under the index the
        // stage's post carried (inside the band): the 19 ETH left are sold
        // $462 under their mark.
        exchange.setMark(ETH, ETH_PX * 991 / 1000);
        exchange.process();
        exchange.setMark(ETH, ETH_PX);
        assertEq(exchange.size(_master(), ETH), 0, "the whole position is gone");
        _nextBlock();
        skip(lighter.SETTLE_DELAY());
        _post();
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
        skip(lighter.DELIVERY_WINDOW());
        _post();
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "the stage lapsed");
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "nothing paid on it: her claim waits");
        assertEq(usd.balanceOf(alice), 0);
        assertEq(exchange.withdrawalCount(), 0, "nothing asked of the exchange");

        // A later stage closes her claim from the accounts as they are.
        value(vault);
        _post();
        vault.settleClaim(1);
        if (vault.claimAt(1).state == ClaimState.Closing) _settleStaged();
        if (vault.claimAt(1).state == ClaimState.Queued) {
            skip(1 minutes);
            _post();
            value(vault);
        }
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "her claim is paid by a later stage");

        _nextBlock();
        _post();
        value(vault);
        // The lapsed stage's fill stays the pool's, so its $462 is shared
        // by both holders: Bob bears no more than his half of it.
        uint256 bobAfter = _worthOf(bob);
        emit log_named_decimal_uint("Bob's loss", bobBefore > bobAfter ? bobBefore - bobAfter : 0, 6);
        assertGe(bobAfter + 240e6, bobBefore, "the stayer bore more than his part of the pool's loss");
    }
}
