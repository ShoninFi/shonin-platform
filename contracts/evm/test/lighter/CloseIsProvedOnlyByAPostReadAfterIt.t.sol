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
/// closed. The close is measured only once the exchange has carried its
/// order out, and that order's fill is the leaver's.
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

    function test_a_claims_own_fill_is_the_leavers_even_when_an_early_post_looks_like_a_proof() public {
        uint256 bobBefore = _stagedBetweenAReadingAndItsPost();
        // The early post proves nothing.
        try station.settleClose() {} catch {}

        // Now the exchange carries her order out, 0.9% under the index the
        // stage's post carried (inside the band): 19 ETH sold $462 under
        // their mark. Whether the close is proved before or after, that fill
        // is hers.
        exchange.setMark(ETH, ETH_PX * 991 / 1000);
        exchange.process();
        exchange.setMark(ETH, ETH_PX);
        _collect();
        for (uint256 i = 0; i < 3 && vault.claimAt(1).state != ClaimState.Paid; i++) {
            _nextBlock();
            skip(lighter.SETTLE_DELAY());
            _post();
            try station.settleClose() {} catch {}
            exchange.process();
            _collect();
        }
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "her claim is paid from its delivery");

        _nextBlock();
        _post();
        value(vault);
        // Alice's transaction fee (about $100) stays with Bob; her order's
        // fill must not be his to pay.
        assertGe(_worthOf(bob) + 1e6, bobBefore, "the stayer must not pay for the leaver's fill");
    }
}
