// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// The constructor holds the timelock to the same ceiling as `setTimelock`.
/// Without it, a deploy script with the wrong unit (milliseconds, say) would
/// ship a registry whose every proposal overflows and which can never be
/// lowered out of it.
contract ConstructorTimelockTest is Fixture {
    function test_the_constructor_holds_the_timelock_to_the_same_ceiling_as_set_timelock() public {
        uint64 tooLong = type(uint64).max - 1 days;
        vm.expectRevert(AttestationRegistry.TooLong.selector);
        new AttestationRegistry(admin, verifier, tooLong, BLESSING_TTL);

        AttestationRegistry longest = new AttestationRegistry(admin, verifier, 365 days, BLESSING_TTL);
        vm.prank(admin);
        longest.proposeImage(keccak256("any image"), ImageKind.Executor);
    }
}
