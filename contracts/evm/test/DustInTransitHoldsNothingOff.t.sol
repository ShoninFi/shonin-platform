// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {StationPost, Transit, ValuationStatus, VaultState} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// Dust in transit in all, what anyone can set moving by sending a stopped
/// station a unit and unwinding it, is counted but never waited for: it
/// neither holds a halted vault's close off nor, late, voids a valuation.
contract DustInTransitHoldsNothingOffTest is TwoChainVault {
    function test_dust_in_transit_neither_holds_the_close_nor_voids_a_valuation() public {
        depositTwo(alice, 100_000e6);
        vm.prank(admin);
        vault.kill();
        skip(1 hours);

        // The other chain's post names two specks on their way home, one of
        // them sent long ago: dust in all.
        uint64 time = open();
        StationPost memory p = postOf(0, 0);
        p.transit = new Transit[](2);
        p.transit[0] = Transit({amount: 1, sentAt: time - 1 minutes});
        p.transit[1] = Transit({amount: MINIMUM_DEPOSIT / 10 - 1, sentAt: time - 3 hours});
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);
        assertTrue(latest().status == ValuationStatus.Priced, "dust in transit voided the valuation");

        vault.close();
        assertTrue(vault.state() == VaultState.Closed, "dust in transit held the close off");
    }

    function test_money_in_transit_above_dust_still_holds_the_close() public {
        depositTwo(alice, 100_000e6);
        vm.prank(admin);
        vault.kill();
        skip(1 hours);

        uint64 time = open();
        StationPost memory p = postOf(0, 0);
        p.transit = new Transit[](1);
        p.transit[0] = Transit({amount: MINIMUM_DEPOSIT / 10 + 1, sentAt: time - 1 minutes});
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);
        assertTrue(latest().status == ValuationStatus.Priced);
        vm.expectRevert();
        vault.close();
    }
}
