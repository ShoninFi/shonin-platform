// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {DevAttestationVerifier} from "../src/DevAttestationVerifier.sol";
import {Fixture} from "./Fixture.sol";

/// No admin-only path wedges an address for good: an address retired or
/// revoked before it was ever proposed can still be proposed later.
contract AdminFootgunsTest is Fixture {
    /// `retireVerifier` sets a flag that `proposeVerifier` never clears. A
    /// checker address retired before it was proposed (a typo, or a retire
    /// sent for the wrong address) is accepted as a proposal, waits out its
    /// timelock, and is then refused forever; `AlreadyProposed` stops a second
    /// try. `proposeProject` has the same rule.
    function test_a_checker_retired_before_it_was_proposed_can_still_be_proposed() public {
        DevAttestationVerifier next = new DevAttestationVerifier(attestor);
        vm.startPrank(admin);
        attestation.retireVerifier(address(next));
        attestation.proposeVerifier(address(next));
        vm.stopPrank();
        skip(TIMELOCK);
        assertTrue(attestation.isVerifierAllowed(address(next)), "the checker is blocked for good");
        vm.prank(admin);
        vm.expectRevert(AttestationRegistry.AlreadyProposed.selector);
        attestation.proposeVerifier(address(next));
    }

    /// `revokeDriver` marks the implementation address revoked for that name
    /// and nothing ever clears it. Proposing the same address again (the
    /// revoke was a mistake, or the problem was elsewhere) is accepted, waits
    /// out a full timelock, becomes the effective implementation -- and is
    /// still refused for trading, while `unwindDriver` returns it. The admin's
    /// only way out is a redeploy at a new address. The same holds for bridges.
    function test_a_revoked_driver_proposed_again_trades_again_after_its_timelock() public {
        address v1 = makeAddr("driver v1");
        vm.startPrank(admin);
        attestation.proposeDriver("local", v1);
        skip(TIMELOCK);
        attestation.revokeDriver("local");
        assertEq(attestation.tradingDriver("local"), address(0));
        attestation.proposeDriver("local", v1);
        vm.stopPrank();
        skip(TIMELOCK);
        assertEq(attestation.unwindDriver("local"), v1);
        assertEq(attestation.tradingDriver("local"), v1, "a re-proposed implementation never trades again");
    }
}
