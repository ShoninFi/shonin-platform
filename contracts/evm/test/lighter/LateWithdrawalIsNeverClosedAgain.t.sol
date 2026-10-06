// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {ClaimState} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";

/// Lighter pays a withdrawal out minutes after it is asked for, sometimes
/// later than the stage's delivery window. A proved close is never placed
/// again: its stage waits for its dollars, asks for them again once a window
/// has passed, and the claim is paid when they land. Nothing is sold twice
/// for one claim, and the stayers' part of the position stays.
contract LateWithdrawalIsNeverClosedAgainTest is LighterVault {
    function test_a_claim_whose_dollars_land_after_the_window_is_never_closed_again_and_paid_when_they_land()
        public
    {
        uint256 aliceShares = _fundAndTrade();
        _aliceClaims(aliceShares);

        // The close is staged: half the position is ordered closed.
        vault.settleClaim(1);
        assertEq(exchange.orderCount(), 1);
        _nextBlock();
        exchange.process();
        int64 halfLeft = exchange.size(_master(), ETH);
        assertApproxEqRel(uint256(int256(halfLeft)), 200_000, 0.01e18, "Alice's half was sold");

        // A newer post proves it; what it fetched is asked for.
        _post();
        station.settleClose();
        assertEq(exchange.withdrawalCount(), 1, "proved and swept: the withdrawal is requested");
        uint256 fetched = station.stage().fetched;

        // The exchange takes longer than the window to carry it out. The
        // stage keeps waiting, and asks again for what hasn't come.
        uint256 cashBefore = vault.cash();
        skip(station.stage().window);
        _nextBlock();
        station.settleClose();
        assertEq(vault.cash(), cashBefore, "nothing came home");
        assertGt(station.stage().stagedAt, 0, "the stage is still waiting");
        assertEq(exchange.withdrawalCount(), 2, "asked for again");
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();

        // The claim can't be tried again meanwhile: nothing is sold twice.
        vm.expectRevert();
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Closing);

        // Now the withdrawals land as a pending balance anyone may collect.
        exchange.process();
        _collect();
        _nextBlock();
        _post();
        assertEq(exchange.size(_master(), ETH), halfLeft, "the stayers' half is never sold for her claim");

        uint256 paidBefore = usd.balanceOf(alice);
        station.settleClose();
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "paid when its dollars landed");
        assertApproxEqRel(usd.balanceOf(alice) - paidBefore, 100_000e6, 0.01e18);
        assertApproxEqRel(fetched, 75_000e6, 0.01e18);
        // What was asked twice and not hers stays at the station, everyone's.
        (uint256 mark,) = station.value();
        assertApproxEqRel(mark, 75_000e6, 0.01e18, "Bob's half of the account is still counted, once");
    }
}
