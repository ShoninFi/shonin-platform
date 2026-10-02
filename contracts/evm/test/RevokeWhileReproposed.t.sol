// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Fixture} from "./Fixture.sol";

/// The implementation in force can be revoked at once even while the same
/// implementation is proposed again: running stations that pin it stop
/// trading through it.
contract RevokeWhileReproposedTest is Fixture {
    function test_a_pinned_driver_proposed_again_is_revoked_at_once_while_proposed_again() public {
        (Vault vault,) = openVault();
        Station station = stationOf(vault);
        assertEq(station.pinnedDriver("local"), address(driver));
        depositAs(vault, alice, 100_000e6);

        // The admin proposes the implementation in force again (a refresh,
        // a mistake, or a second proposal that happens to name it).
        vm.prank(admin);
        attestation.proposeDriver("local", address(driver));
        assertEq(attestation.tradingDriver("local"), address(driver), "the driver in force changed");

        // Then needs it gone at once.
        vm.prank(admin);
        attestation.revokeDriver("local", address(driver));
        assertTrue(attestation.isDriverRevoked("local", address(driver)), "the pinned driver is not revoked");

        vm.prank(executor);
        vm.expectRevert(Station.NotApproved.selector);
        station.approveKey("local", executor);
    }

    function test_a_pinned_bridge_proposed_again_is_revoked_at_once_while_proposed_again() public {
        vm.startPrank(admin);
        attestation.proposeBridge("local-bridge", address(bridge));
        attestation.revokeBridge("local-bridge", address(bridge));
        vm.stopPrank();
        assertTrue(attestation.isBridgeRevoked("local-bridge", address(bridge)));
    }
}
