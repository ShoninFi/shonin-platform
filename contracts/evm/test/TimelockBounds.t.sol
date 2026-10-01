// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// The timelock has a ceiling. Without one, a value near the top of `uint64`
/// would overflow every proposal and every attempt to lower it again, so one
/// admin transaction could stop anything ever being added to the registry.
contract TimelockBoundsTest is Fixture {
    function test_a_timelock_near_the_top_cannot_be_set() public {
        vm.prank(admin);
        // Expected: refused as out of range. Actual: accepted.
        vm.expectRevert();
        attestation.setTimelock(type(uint64).max);
    }

    function test_the_timelock_has_a_ceiling_and_can_always_be_lowered_again() public {
        vm.startPrank(admin);
        vm.expectRevert(AttestationRegistry.TooLong.selector);
        attestation.setTimelock(type(uint64).max);
        uint64 ceiling = attestation.MAX_TIMELOCK();
        attestation.setTimelock(ceiling);
        attestation.proposeImage(keccak256("any image"), ImageKind.Executor);
        attestation.setTimelock(TIMELOCK);
        vm.stopPrank();
        skip(ceiling);
        assertEq(attestation.timelock(), TIMELOCK);
    }
}
