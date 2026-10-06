// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {ClaimState, ValuationStatus} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";

/// Lighter pays a withdrawal out minutes after it is asked for, and could
/// take longer than the stage's delivery window. A stage never waits past
/// its window: it delivers what has landed (here nothing), the claim waits
/// with the shares it wasn't paid for, and the vault prices again. Dollars
/// that land afterwards are everyone's, the waiting claim's shares
/// included, and count once.
contract LateWithdrawalEndsTheStageTest is LighterVault {
    function test_a_close_whose_dollars_have_not_landed_a_window_after_the_ask_ends_and_the_claim_waits()
        public
    {
        uint256 aliceShares = _fundAndTrade();
        _aliceClaims(aliceShares);

        vault.settleClaim(1);
        assertEq(exchange.orderCount(), 1);
        _nextBlock();
        exchange.process();
        int64 halfLeft = exchange.size(_master(), ETH);
        assertApproxEqRel(uint256(int256(halfLeft)), 200_000, 0.01e18, "Alice's half was sold");

        // A post read after the settle delay proves it; what it fetched is
        // asked for.
        skip(lighter.SETTLE_DELAY());
        _post();
        station.settleClose();
        assertEq(exchange.withdrawalCount(), 1, "proved and swept: the withdrawal is requested");
        assertApproxEqRel(station.stage().fetched, 75_000e6, 0.01e18);

        // The exchange takes longer than the window to carry it out.
        uint256 cashBefore = vault.cash();
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
        skip(station.stage().window);
        _nextBlock();
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "the stage has ended");
        assertEq(exchange.withdrawalCount(), 1, "nothing is asked again");
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "the claim waits");
        assertEq(vault.claimAt(1).shares, aliceShares, "with the shares nothing was paid for");
        assertGt(vault.cash(), cashBefore, "its part of the cash is back in the cash");

        // The vault prices again, the late dollars included once they land.
        exchange.process();
        _collect();
        _nextBlock();
        _post();
        value(vault);
        uint256 index = vault.valuationCount() - 1;
        assertTrue(vault.valuationAt(index).status == ValuationStatus.Priced, "the vault prices again");
        assertEq(exchange.size(_master(), ETH), halfLeft, "nothing more is sold meanwhile");
        (uint256 mark,) = station.value();
        assertApproxEqRel(mark, 150_000e6, 0.01e18, "the whole account, counted once");
    }
}
