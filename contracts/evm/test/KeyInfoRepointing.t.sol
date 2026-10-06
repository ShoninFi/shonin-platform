// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {KeyInfo} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// What `keyInfo` reports a key as serving moves only with a later blessing,
/// so a stranger replaying a public seal with the other leaf of a two-vault
/// journal can't change what the view and the vault's page say.
contract KeyInfoRepointingTest is Fixture {
    address internal vaultA = makeAddr("vault A");
    address internal vaultB = makeAddr("vault B");
    bytes32 internal bundle = keccak256("bundle");

    function pair(bytes32 a, bytes32 b) internal pure returns (bytes32) {
        return a < b ? keccak256(abi.encodePacked(a, b)) : keccak256(abi.encodePacked(b, a));
    }

    function test_a_stranger_cannot_change_what_a_key_is_reported_as_serving() public {
        bytes32 leafA = leafFor(vaultA, bundle, executor);
        bytes32 leafB = leafFor(vaultB, bundle, executor);
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, pair(leafA, leafB));
        bytes memory seal = sealOf(journal);
        bytes32[] memory proofA = new bytes32[](1);
        proofA[0] = leafB;
        bytes32[] memory proofB = new bytes32[](1);
        proofB[0] = leafA;

        attestation.bless(seal, journal, block.chainid, vaultA, bundle, executor, proofA);
        KeyInfo memory before = attestation.keyInfo(executor);
        assertEq(before.vault, vaultA);

        vm.prank(alice);
        attestation.bless(seal, journal, block.chainid, vaultB, bundle, executor, proofB);
        KeyInfo memory after_ = attestation.keyInfo(executor);
        // The blessing for A stands ...
        assertTrue(attestation.isBlessedFor(executor, block.chainid, vaultA, bundle));
        // ...but the record now says the key serves B, by a stranger's hand.
        assertEq(after_.vault, before.vault, "a passer-by re-pointed keyInfo().vault");
    }
}
