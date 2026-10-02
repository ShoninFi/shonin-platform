// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {Station} from "../src/Station.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A revocation withdraws any waiting proposal of the same implementation,
/// so it can't lapse when that proposal would have come due.
contract RevokeOutlastsAnEarlierProposalTest is ClaimBase {
    function test_a_revoke_made_while_the_driver_is_reproposed_still_holds_after_the_timelock() public {
        depositAs(vault, alice, 100_000e6);
        assertEq(station.pinnedDriver("local"), address(driver));

        // Day 0: the admin proposes the implementation in force again.
        vm.prank(admin);
        attestation.proposeDriver("local", address(driver));

        // Day 1: the admin needs it gone at once (the venue changed, a bug was
        // found) and revokes it, as the handbook says to.
        skip(1 days);
        vm.prank(admin);
        attestation.revokeDriver("local", address(driver));
        assertTrue(attestation.isDriverRevoked("local", address(driver)), "revoked at once");
        vm.prank(executor);
        vm.expectRevert(Station.NotApproved.selector);
        station.approveKey("local", executor);

        // Day 2: the re-proposal's timelock passes. The admin did nothing
        // else. The revoke should still stand: nothing re-approved it after
        // the revoke, and no new notice was given.
        skip(TIMELOCK);
        assertTrue(
            attestation.isDriverRevoked("local", address(driver)),
            "the revoke lapsed when the earlier re-proposal matured"
        );
        assertEq(attestation.tradingDriver("local"), address(0), "the revoked driver is approved again");
    }

    /// Once the re-proposal has matured the admin can't even withdraw it any
    /// more, and the first thing it does to the slot wipes the revoke flag,
    /// so the only way to make the revoke stick is to revoke a second time
    /// after the maturity, during which window the executor trades through
    /// the driver.
    function test_after_the_lapse_the_executor_trades_through_the_revoked_driver() public {
        depositAs(vault, alice, 100_000e6);
        vm.startPrank(admin);
        attestation.proposeDriver("local", address(driver));
        attestation.revokeDriver("local", address(driver));
        vm.stopPrank();
        skip(TIMELOCK);

        // The proposal can no longer be withdrawn: it has already matured.
        vm.prank(admin);
        vm.expectRevert(AttestationRegistry.NoImplementation.selector);
        attestation.withdrawDriverProposal("local");

        // And money goes into the venue through the "revoked" driver.
        vm.startPrank(executor);
        station.pull(50_000e6);
        vm.expectRevert(Station.NotApproved.selector);
        station.deposit("local", 50_000e6);
        vm.stopPrank();
    }
}
