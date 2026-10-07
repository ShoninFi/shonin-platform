// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {DevAttestationVerifier} from "../src/DevAttestationVerifier.sol";

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// Retiring an attestation checker reaches every key it blessed at once, as
/// revoking an image or a project does, so a checker found to accept forged
/// proofs can be removed without stopping every honest key from the same
/// image or project.
contract VerifierRetirementTest is Fixture {
    uint256 internal constant LEAKED_KEY = 0x1EA4;

    DevAttestationVerifier internal leaked;

    function setUp() public override {
        super.setUp();
        // A second checker, allowed after its timelock, whose signing key
        // later turns out to be in the wrong hands.
        leaked = new DevAttestationVerifier(vm.addr(LEAKED_KEY));
        vm.prank(admin);
        attestation.proposeVerifier(address(leaked));
        skip(TIMELOCK);
        // The builder and reviewer blessings from the fixture have aged by a
        // timelock; refresh them so `openVault` still works.
        blessServiceKey(BUILDER_IMAGE, builder);
        blessServiceKey(REVIEWER_IMAGE, reviewer);
    }

    function forgedSeal(bytes memory journal) internal pure returns (bytes memory) {
        return signed(LEAKED_KEY, keccak256(journal));
    }

    function test_retiring_a_checker_stops_the_keys_it_blessed() public {
        (Vault vault, bytes32 bundle) = openVault();
        Station station = stationOf(vault);
        address forged = makeAddr("forged executor");

        // Whoever holds the leaked key attests any journal they like: the
        // image and project ids are public, and the key is their own.
        bytes memory journal = journalFor(EXECUTOR_IMAGE, forged, leafFor(address(vault), bundle, forged));
        attestation.bless(
            forgedSeal(journal), journal, block.chainid, address(vault), bundle, forged, new bytes32[](0)
        );
        assertTrue(attestation.isBlessedFor(forged, block.chainid, address(vault), bundle));

        // And the vault lets that key trade the depositors' account.
        station.approveKey("local", forged);
        assertTrue(venue.isAgent(address(station), forged));

        // The admin finds out and retires the checker at once.
        vm.prank(admin);
        attestation.retireVerifier(address(leaked));
        assertFalse(attestation.isVerifierAllowed(address(leaked)));

        assertFalse(
            attestation.isBlessedFor(forged, block.chainid, address(vault), bundle),
            "a key blessed through a retired checker still counts"
        );
        // Which is what lets anyone cut it off at the venue.
        vm.prank(alice);
        station.revokeKey("local", forged);
        assertFalse(
            venue.isAgent(address(station), forged), "the forged key still trades after the retirement"
        );
    }

    function test_retiring_a_checker_stops_the_service_keys_it_blessed() public {
        address forgedBuilder = makeAddr("forged builder");
        bytes memory journal = journalFor(BUILDER_IMAGE, forgedBuilder, serviceLeaf(forgedBuilder));
        attestation.bless(
            forgedSeal(journal), journal, 0, address(0), bytes32(0), forgedBuilder, new bytes32[](0)
        );
        assertTrue(attestation.isBlessedAs(forgedBuilder, ImageKind.Builder));

        vm.prank(admin);
        attestation.retireVerifier(address(leaked));

        assertFalse(
            attestation.isBlessedAs(forgedBuilder, ImageKind.Builder),
            "a builder blessed through a retired checker can still sign registrations"
        );
    }
}
