// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// Money the station's account parked in one of the venue's own vaults is
/// counted in the station's value once it is reclaimed, and every holder
/// owns their part of it, but no close can sell it. So no claim is closed
/// while it is parked: the claim waits, nothing is sold, and once the money
/// is home the claim is paid its part of it, the stayers none of it.
contract ALeaverKeepsTheirPartOfMoneyParkedInAVenueVaultTest is HyperliquidVault {
    function test_a_claim_waits_while_money_is_parked_and_is_then_paid_the_leavers_part_of_it() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();

        // The account's key parks 8k of the account's free dollars in a
        // venue vault, locked for a day; anyone has the station count it.
        address venueVault = address(0xBEEF);
        core.parkInVault(address(station), venueVault, 8000e6, uint64((block.timestamp + 1 days) * 1000));
        station.reclaim(HL, venueVault);

        value(vault);
        uint256 aliceBefore = _worthOf(alice);
        uint256 bobBefore = _worthOf(bob);
        // Each owns half of everything, the parked 8k included.
        assertApproxEqRel(aliceBefore, 100_000e6, 0.01e18);

        _aliceClaims(aliceShares);
        (uint256 markBefore,) = station.value();
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "her close waits while money is parked");
        assertEq(station.stage().stagedAt, 0, "nothing is staged");
        (uint256 markAfter,) = station.value();
        assertEq(markAfter, markBefore, "nothing is sold");

        // The lock ends and the money comes home; then her claim is closed.
        skip(1 days);
        station.reclaim(HL, venueVault);
        _nextBlock();
        assertEq(core.vaultEquity(address(station), venueVault), 0, "the money is home");
        value(vault);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Closing, "her close is staged");
        _nextBlock();
        skip(hl.SETTLE_DELAY());
        station.settleClose();
        _nextBlock();
        // Delivered once its dollars have landed, or with what has landed
        // once its delivery window is over.
        if (station.stage().stagedAt != 0) {
            try station.settleClose() {}
            catch {
                skip(station.stage().window);
                station.settleClose();
            }
        }
        assertEq(station.stage().stagedAt, 0, "the stage has ended");

        uint256 paid = usd.balanceOf(alice);
        uint256 left = vault.claimAt(1).shares;
        if (vault.claimAt(1).state == ClaimState.Paid) left = 0;
        value(vault);
        uint256 bobAfter = _worthOf(bob);

        // Whatever she was not paid for, she still holds shares for: what
        // she was paid and what her remaining shares are worth come to what
        // she owned, give or take the spread and the fees (a few hundred dollars).
        emit log_named_decimal_uint("she owned", aliceBefore, 6);
        emit log_named_decimal_uint("she was paid", paid, 6);
        emit log_named_uint("shares left of 1e6", left * 1e6 / aliceShares);
        emit log_named_decimal_uint("the stayer before", bobBefore, 6);
        emit log_named_decimal_uint("the stayer after", bobAfter, 6);
        uint256 kept = left == 0 ? 0 : left * bobAfter / vault.balanceOf(bob);
        assertGe(paid + kept + 1000e6, aliceBefore, "the leaver lost her part of the parked money");
        // And the stayer is not made richer by it.
        assertLe(bobAfter, bobBefore + 1000e6, "the stayer took the leaver's part of the parked money");
    }

    /// A stopped station's full unwind is everyone's, so money parked out of
    /// a close's reach doesn't hold it: what the account holds is sold.
    function test_a_stopped_stations_full_unwind_is_not_held_by_money_parked_in_a_venue_vault() public {
        _fundAndTrade();
        _stockGas();
        address venueVault = address(0xBEEF);
        core.parkInVault(address(station), venueVault, 8000e6, uint64((block.timestamp + 1 days) * 1000));
        station.reclaim(HL, venueVault);
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();

        station.unwind(1e18);
        _nextBlock();
        assertEq(core.szi(address(station), ETH), 0, "the account's position was closed");
    }
}
