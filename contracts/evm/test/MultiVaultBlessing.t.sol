// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Fixture} from "./Fixture.sol";

/// A key whose attested root covers several vaults is blessed for each of
/// them separately, so nobody replaying the public seal with another leaf can
/// move the key off the vault it is serving.
///
/// Each test states what holds.
contract MultiVaultBlessingTest is Fixture {
    address internal vaultA = makeAddr("vault A");
    address internal vaultB = makeAddr("vault B");
    bytes32 internal bundle = keccak256("bundle");

    function pair(bytes32 a, bytes32 b) internal pure returns (bytes32) {
        return a < b ? keccak256(abi.encodePacked(a, b)) : keccak256(abi.encodePacked(b, a));
    }

    /// A journal over two leaves, and the proof for each.
    function twoVaultJournal(
        address signer,
        address first,
        address second
    )
        internal
        view
        returns (bytes memory journal, bytes32[] memory proofFirst, bytes32[] memory proofSecond)
    {
        bytes32 leafFirst = leafFor(first, bundle, signer);
        bytes32 leafSecond = leafFor(second, bundle, signer);
        journal = journalFor(EXECUTOR_IMAGE, signer, pair(leafFirst, leafSecond));
        proofFirst = new bytes32[](1);
        proofFirst[0] = leafSecond;
        proofSecond = new bytes32[](1);
        proofSecond[0] = leafFirst;
    }

    /// An instance attested for two vaults should hold a key that serves both.
    function test_a_key_attested_for_two_vaults_serves_both() public {
        (bytes memory journal, bytes32[] memory proofA, bytes32[] memory proofB) =
            twoVaultJournal(executor, vaultA, vaultB);
        bytes memory seal = sealOf(journal);

        attestation.bless(seal, journal, block.chainid, vaultA, bundle, executor, proofA);
        assertTrue(attestation.isBlessedFor(executor, block.chainid, vaultA, bundle));

        attestation.bless(seal, journal, block.chainid, vaultB, bundle, executor, proofB);
        assertTrue(attestation.isBlessedFor(executor, block.chainid, vaultB, bundle));
        assertTrue(
            attestation.isBlessedFor(executor, block.chainid, vaultA, bundle),
            "blessing for vault B un-blessed the key for vault A"
        );
    }

    /// Nobody but the instance should be able to change what its key is
    /// blessed for. Here a stranger replays the public seal with the other
    /// leaf and the key stops serving the vault it was working for.
    function test_a_stranger_cannot_move_a_key_to_another_vault() public {
        (bytes memory journal, bytes32[] memory proofA, bytes32[] memory proofB) =
            twoVaultJournal(executor, vaultA, vaultB);
        bytes memory seal = sealOf(journal);
        attestation.bless(seal, journal, block.chainid, vaultA, bundle, executor, proofA);

        // Everything alice needs was in the calldata of the first blessing.
        vm.prank(alice);
        attestation.bless(seal, journal, block.chainid, vaultB, bundle, executor, proofB);

        assertTrue(
            attestation.isBlessedFor(executor, block.chainid, vaultA, bundle),
            "a passer-by un-blessed the executor for vault A"
        );
    }

    /// The same, end to end through the vault: the executor is approved as the
    /// venue agent, a stranger flips its blessing, and then the stranger (or
    /// anyone) revokes the agent and the vault stops trading.
    function test_a_stranger_cannot_cut_off_a_live_vaults_executor() public {
        (Vault vault, bytes32 liveBundle) = openVault();
        Station station = stationOf(vault);
        bundle = liveBundle;
        (bytes memory journal, bytes32[] memory proofLive, bytes32[] memory proofOther) =
            twoVaultJournal(executor, address(vault), vaultB);
        bytes memory seal = sealOf(journal);
        attestation.bless(seal, journal, block.chainid, address(vault), bundle, executor, proofLive);
        station.approveKey("local", executor);
        assertTrue(venue.isAgent(address(station), executor));

        // A stranger may not revoke a key whose blessing stands...
        vm.prank(alice);
        vm.expectRevert(Station.StillBlessed.selector);
        station.revokeKey("local", executor);

        // ...but can make it not stand, with the public seal and the other leaf.
        vm.startPrank(alice);
        attestation.bless(seal, journal, block.chainid, vaultB, bundle, executor, proofOther);
        vm.expectRevert(Station.StillBlessed.selector);
        station.revokeKey("local", executor);
        vm.stopPrank();
        assertTrue(venue.isAgent(address(station), executor), "a stranger revoked the live executor");
    }
}
