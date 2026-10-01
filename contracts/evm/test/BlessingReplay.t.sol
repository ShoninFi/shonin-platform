// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// A blessing runs for its TTL from the moment the instance attested, so
/// replaying a public seal can never extend it, and revoking an operator
/// project reaches every key that project holds.
contract BlessingReplayTest is Fixture {
    function test_a_lapsed_blessing_cannot_be_renewed_by_replaying_the_old_seal() public {
        address vault = makeAddr("vault");
        bytes32 bundle = keccak256("bundle");
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, leafFor(vault, bundle));
        bytes memory seal = sealOf(journal);
        attestation.bless(seal, journal, block.chainid, vault, bundle, new bytes32[](0));

        skip(BLESSING_TTL + 1);
        assertFalse(attestation.isBlessedFor(executor, block.chainid, vault, bundle));

        // Months later, a stranger replays the first blessing's calldata.
        skip(90 days);
        vm.prank(alice);
        vm.expectRevert(AttestationRegistry.Stale.selector);
        attestation.bless(seal, journal, block.chainid, vault, bundle, new bytes32[](0));

        assertFalse(
            attestation.isBlessedFor(executor, block.chainid, vault, bundle),
            "an old seal renewed a lapsed blessing"
        );
    }

    /// The registry says removing anything is immediate because the reason is
    /// always urgent. Revoking an image does reach its keys at once; revoking
    /// a project does not, and its keys keep counting until their TTL runs
    /// out (and, with the replay above, beyond it).
    function test_revoking_a_project_stops_its_keys_at_once() public {
        assertTrue(attestation.isBlessedAs(builder, ImageKind.Builder));
        assertTrue(attestation.isBlessedAs(reviewer, ImageKind.Reviewer));

        vm.prank(admin);
        attestation.revokeProject(PROJECT);
        assertFalse(attestation.isProjectAllowed(PROJECT));

        assertFalse(attestation.isBlessedAs(builder, ImageKind.Builder), "builder key outlived its project");
        assertFalse(
            attestation.isBlessedAs(reviewer, ImageKind.Reviewer), "reviewer key outlived its project"
        );
    }
}
