// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Fixture} from "./Fixture.sol";

/// While the executor key in place is still blessed, only the key being
/// approved can take its place at the venue: during a handover, when two
/// instances are blessed at once, nobody else can swap the venue between
/// them, in either direction.
contract HandoverCannotBeFlippedTest is Fixture {
    uint256 internal constant OTHER_EXECUTOR_KEY = 0xE8ED;
    address internal otherExecutor = vm.addr(OTHER_EXECUTOR_KEY);

    function test_a_passer_by_cannot_unseat_a_still_blessed_executor() public {
        (Vault vault, bytes32 bundle) = openVault();
        Station station = stationOf(vault);

        // The running instance approves itself.
        vm.prank(executor);
        station.approveKey("local", executor);
        assertTrue(venue.isAgent(address(station), executor));

        // A rotation: a second instance is blessed for the same vault, and
        // the first one's blessing has days left to run.
        blessExecutorKey(otherExecutor, address(vault), bundle);

        // A stranger swaps the venue's agent to the other key.
        vm.prank(alice);
        vm.expectRevert(Station.NotAllowed.selector);
        station.approveKey("local", otherExecutor);

        assertTrue(
            venue.isAgent(address(station), executor),
            "a passer-by revoked the running executor's venue approval by re-approving another blessed key"
        );
    }

    /// The same, the other way round, to show it is a loop: once the new
    /// instance has taken over, anyone can put the old key back.
    function test_a_passer_by_cannot_put_a_retired_key_back() public {
        (Vault vault, bytes32 bundle) = openVault();
        Station station = stationOf(vault);
        blessExecutorKey(otherExecutor, address(vault), bundle);

        // The new instance takes over, as a rotation does.
        vm.prank(otherExecutor);
        station.approveKey("local", otherExecutor);
        assertTrue(venue.isAgent(address(station), otherExecutor));
        assertFalse(venue.isAgent(address(station), executor));

        // The old key is still blessed, so a stranger can hand the venue
        // account back to it, kicking the new instance out.
        vm.prank(alice);
        vm.expectRevert(Station.NotAllowed.selector);
        station.approveKey("local", executor);

        assertTrue(
            venue.isAgent(address(station), otherExecutor),
            "a passer-by unseated the new instance by re-approving the old, still-blessed key"
        );
    }
}
