// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {IAttestationVerifier} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// The constructor refuses a zero checker, as `proposeVerifier` does, so a
/// registry is never deployed listing `address(0)` as allowed.
contract ZeroVerifierConstructorTest is Fixture {
    function test_the_constructor_refuses_a_zero_checker_like_propose_does() public {
        vm.expectRevert(AttestationRegistry.NoImplementation.selector);
        new AttestationRegistry(admin, IAttestationVerifier(address(0)), TIMELOCK, BLESSING_TTL);
    }
}
