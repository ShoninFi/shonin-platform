// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// A checker that burns every bit of gas it is handed.
contract GasBurningChecker {
    function verify(bytes calldata, bytes calldata) external pure returns (bool) {
        uint256 i;
        while (true) {
            unchecked {
                ++i;
            }
        }
        return i == 0;
    }
}

/// An allowed checker that burns its gas on every proof is given only its
/// fixed budget, so an honest proof still reaches the checker that accepts
/// it at an ordinary gas limit: one bad checker cannot make every blessing
/// fail.
contract GasBurningCheckerCannotBlockBlessingsTest is Fixture {
    GasBurningChecker internal burner;

    function setUp() public override {
        super.setUp();
        burner = new GasBurningChecker();
        vm.prank(admin);
        attestation.proposeVerifier(address(burner));
        skip(TIMELOCK);
        // The newest allowed checker: it is asked first.
        assertTrue(attestation.isVerifierAllowed(address(burner)));
    }

    function test_a_blessing_goes_through_at_an_ordinary_budget_past_a_checker_that_burns_its_gas() public {
        address vault = makeAddr("vault");
        bytes32 bundle = keccak256("bundle");
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, leafFor(vault, bundle));
        bytes memory call = abi.encodeCall(
            AttestationRegistry.bless,
            (sealOf(journal), journal, block.chainid, vault, bundle, new bytes32[](0))
        );
        // The burner's budget plus room for everything else, but well short
        // of what the burner would swallow if it were handed it all.
        uint256 budget = attestation.VERIFIER_GAS() + 500_000;
        (bool ok,) = address(attestation).call{gas: budget}(call);
        assertTrue(ok, "a checker that burns its gas made an honest blessing fail");
        assertTrue(attestation.isBlessedFor(executor, block.chainid, vault, bundle));
    }
}
