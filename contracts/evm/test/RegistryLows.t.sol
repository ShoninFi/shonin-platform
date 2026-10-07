// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";
import {Vm} from "forge-std/Vm.sol";

/// Hardening with no money behind it: no zero checker, and a replay that
/// changes nothing announces what is recorded.
contract RegistryLowsTest is Fixture {
    /// A checker, like a driver or a bridge, is never the zero address:
    /// otherwise it would sit in the list for good, read as allowed, and be
    /// tried on every blessing.
    function test_the_zero_address_cannot_be_proposed_as_a_checker() public {
        vm.prank(admin);
        vm.expectRevert();
        attestation.proposeVerifier(address(0));
    }

    /// Replaying a public seal that is not later than the blessing already
    /// recorded changes nothing, and the event says what is recorded, so an
    /// indexer that reads expiries from events never shows an earlier one.
    function test_a_replay_that_changes_nothing_does_not_announce_an_earlier_expiry() public {
        address vault = makeAddr("vault");
        bytes32 bundle = keccak256("bundle");
        bytes memory first = journalFor(EXECUTOR_IMAGE, executor, leafFor(vault, bundle, executor));
        bytes memory firstSeal = sealOf(first);
        attestation.bless(firstSeal, first, block.chainid, vault, bundle, executor, new bytes32[](0));
        uint64 firstUntil = uint64(vm.getBlockTimestamp()) + BLESSING_TTL;

        // The instance attests again a day later; its blessing moves later.
        skip(1 days);
        blessExecutorKey(executor, vault, bundle);
        uint64 later = uint64(vm.getBlockTimestamp()) + BLESSING_TTL;
        assertEq(attestation.blessedUntil(executor, block.chainid, vault, bundle), later);

        // A stranger replays the first seal. The chain keeps `later`...
        vm.recordLogs();
        vm.prank(alice);
        attestation.bless(firstSeal, first, block.chainid, vault, bundle, executor, new bytes32[](0));
        assertEq(attestation.blessedUntil(executor, block.chainid, vault, bundle), later);

        // ...and announces `later`, never `firstUntil`.
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] != keccak256("Blessed(address,uint256,address,bytes32,bytes32,uint64)")) {
                continue;
            }
            (,,, uint64 announced) = abi.decode(logs[i].data, (uint256, bytes32, bytes32, uint64));
            assertEq(announced, later, "a no-op replay announced the older, shorter expiry");
            assertTrue(announced != firstUntil);
        }
    }
}
