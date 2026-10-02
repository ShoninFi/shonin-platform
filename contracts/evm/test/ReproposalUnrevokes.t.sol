// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Fixture} from "./Fixture.sol";

/// Proposing a revoked driver or bridge again is a fresh approval, and waits
/// a full timelock like any other. The revocation holds against what is in
/// force until the new proposal is ready, so an outsider who saw the
/// revocation and relied on it gets the same notice before that code trades
/// depositors' money again as for anything new: adding waits, and only
/// removing is immediate.
contract ReproposalUnrevokesTest is Fixture {
    address internal v1 = makeAddr("driver v1");
    address internal v2 = makeAddr("driver v2");

    function test_a_revoked_driver_proposed_again_waits_its_timelock_before_trading() public {
        vm.startPrank(admin);
        attestation.proposeDriver("local", v1);
        skip(TIMELOCK);
        assertEq(attestation.tradingDriver("local"), v1);

        attestation.revokeDriver("local", v1);
        assertEq(attestation.tradingDriver("local"), address(0));

        // The admin changes its mind (or a compromised admin key wants it
        // back). The proposal is announced as ready at now + TIMELOCK...
        attestation.proposeDriver("local", v1);
        vm.stopPrank();

        // ...and does not trade until it has waited.
        assertEq(
            attestation.tradingDriver("local"), address(0), "a revoked driver trades again with no timelock"
        );
        skip(TIMELOCK - 1);
        assertEq(
            attestation.tradingDriver("local"), address(0), "a revoked driver trades before its timelock"
        );
        skip(1);
        assertEq(attestation.tradingDriver("local"), v1);
    }

    /// The same when a waiting proposal was withdrawn first: the admin had
    /// v1 in force and v2 waiting, withdrew v2, revoked v1, then put v1 back.
    function test_the_same_after_a_revocation_that_withdrew_a_waiting_proposal() public {
        vm.startPrank(admin);
        attestation.proposeDriver("local", v1);
        skip(TIMELOCK);
        attestation.proposeDriver("local", v2);
        attestation.withdrawDriverProposal("local");
        attestation.revokeDriver("local", v1);
        assertEq(attestation.tradingDriver("local"), address(0));
        attestation.proposeDriver("local", v1);
        vm.stopPrank();
        assertEq(attestation.tradingDriver("local"), address(0), "v1 is back before its timelock");
    }

    function test_a_revoked_bridge_proposed_again_waits_its_timelock() public {
        address bridge = makeAddr("bridge");
        vm.startPrank(admin);
        attestation.proposeBridge("local-bridge", bridge);
        skip(TIMELOCK);
        assertEq(attestation.bridgeOf("local-bridge"), bridge);
        attestation.revokeBridge("local-bridge", bridge);
        assertEq(attestation.bridgeOf("local-bridge"), address(0));
        attestation.proposeBridge("local-bridge", bridge);
        vm.stopPrank();
        assertEq(
            attestation.bridgeOf("local-bridge"), address(0), "a revoked bridge is back with no timelock"
        );
        skip(TIMELOCK);
        assertEq(attestation.bridgeOf("local-bridge"), bridge);
    }
}
