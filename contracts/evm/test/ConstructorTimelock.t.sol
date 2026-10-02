// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// The constructor holds the timelock to the same ceiling as `setTimelock`,
/// so a deploy script with the wrong unit (milliseconds, say) can't ship a
/// registry whose every proposal overflows. A deployment's floor is fixed
/// when it is made: the timelock never starts below it, and the admin can
/// never lower it below it.
contract ConstructorTimelockTest is Fixture {
    function test_the_constructor_holds_the_timelock_to_the_same_ceiling_as_set_timelock() public {
        uint64 tooLong = type(uint64).max - 1 days;
        vm.expectRevert(AttestationRegistry.TooLong.selector);
        new AttestationRegistry(admin, verifier, tooLong, 0, BLESSING_TTL);

        AttestationRegistry longest = new AttestationRegistry(admin, verifier, 365 days, 0, BLESSING_TTL);
        vm.prank(admin);
        longest.proposeImage(keccak256("any image"), ImageKind.Executor);
    }

    function test_the_timelock_never_goes_below_the_deployments_floor() public {
        vm.expectRevert(AttestationRegistry.TooShort.selector);
        new AttestationRegistry(admin, verifier, 1 days, 2 days, BLESSING_TTL);

        AttestationRegistry guarded = new AttestationRegistry(admin, verifier, 2 days, 2 days, BLESSING_TTL);
        vm.startPrank(admin);
        vm.expectRevert(AttestationRegistry.TooShort.selector);
        guarded.setTimelock(0);
        vm.expectRevert(AttestationRegistry.TooShort.selector);
        guarded.setTimelock(2 days - 1);
        guarded.setTimelock(3 days);
        vm.stopPrank();
        assertEq(guarded.timelock(), 3 days);
    }

    function test_a_development_deployment_can_have_no_timelock() public {
        AttestationRegistry dev = new AttestationRegistry(admin, verifier, 0, 0, BLESSING_TTL);
        vm.prank(admin);
        dev.proposeImage(keccak256("any image"), ImageKind.Executor);
        assertTrue(dev.isImageAllowed(keccak256("any image")));
    }
}
