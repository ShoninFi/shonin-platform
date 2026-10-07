// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// A holder image holds the root key and nothing else: no key of its is ever
/// blessed, so no check that asks for a blessed key ever takes one, and the
/// one thing it does on chain is record the root.
///
/// Each test states what holds.
contract HolderKeyAttestationTest is Fixture {
    bytes32 internal constant HOLDER_IMAGE = keccak256("holder image");

    address internal holderInstance = makeAddr("holder instance key");
    address internal holderKey = makeAddr("holder key");

    function setUp() public override {
        super.setUp();
        vm.prank(admin);
        attestation.proposeImage(HOLDER_IMAGE, ImageKind.Holder);
        skip(TIMELOCK);
        blessServiceKey(BUILDER_IMAGE, builder);
        blessServiceKey(REVIEWER_IMAGE, reviewer);
    }

    function test_holder_is_appended_after_every_existing_kind() public pure {
        assertEq(uint8(ImageKind.None), 0);
        assertEq(uint8(ImageKind.Executor), 1);
        assertEq(uint8(ImageKind.Unwinder), 2);
        assertEq(uint8(ImageKind.Builder), 3);
        assertEq(uint8(ImageKind.Reviewer), 4);
        assertEq(uint8(ImageKind.Holder), 5);
    }

    function test_a_holder_image_blesses_no_key() public {
        bytes memory journal = journalFor(HOLDER_IMAGE, holderInstance, serviceLeaf(holderKey));
        bytes memory seal = sealOf(journal);
        vm.expectRevert(AttestationRegistry.HolderBlessesNothing.selector);
        attestation.bless(seal, journal, 0, address(0), bytes32(0), holderKey, new bytes32[](0));
        // Nor the instance's own key, under the leaf a builder would use.
        journal = journalFor(HOLDER_IMAGE, holderInstance, serviceLeaf(holderInstance));
        seal = sealOf(journal);
        vm.expectRevert(AttestationRegistry.HolderBlessesNothing.selector);
        attestation.bless(seal, journal, 0, address(0), bytes32(0), holderInstance, new bytes32[](0));

        assertTrue(attestation.keyInfo(holderKey).kind == ImageKind.None);
        assertTrue(attestation.keyInfo(holderInstance).kind == ImageKind.None);
        assertFalse(attestation.isBlessedAs(holderKey, ImageKind.Holder));
        assertFalse(attestation.isBlessedAs(holderInstance, ImageKind.Holder));
    }

    function test_a_holder_image_cannot_bless_a_key_for_a_vault() public {
        address vault = makeAddr("a vault");
        bytes32 bundle = keccak256("bundle");
        bytes memory journal = journalFor(HOLDER_IMAGE, holderInstance, leafFor(vault, bundle, holderKey));
        bytes memory seal = sealOf(journal);
        vm.expectRevert(AttestationRegistry.HolderBlessesNothing.selector);
        attestation.bless(seal, journal, block.chainid, vault, bundle, holderKey, new bytes32[](0));
        assertFalse(attestation.isBlessedFor(holderKey, block.chainid, vault, bundle));
        assertTrue(attestation.keyInfo(holderKey).kind == ImageKind.None);
    }

    function test_a_station_refuses_a_holder_s_key_as_a_trading_key() public {
        (Vault vault,) = openVault();
        Station station = stationOf(vault);
        bytes memory journal = journalFor(HOLDER_IMAGE, holderInstance, serviceLeaf(holderKey));
        bytes memory seal = sealOf(journal);
        vm.expectRevert(AttestationRegistry.HolderBlessesNothing.selector);
        attestation.bless(seal, journal, 0, address(0), bytes32(0), holderKey, new bytes32[](0));

        vm.expectRevert(Station.NotBlessed.selector);
        station.approveKey("local", holderKey);
        assertFalse(venue.isAgent(address(station), holderKey));
    }

    function test_a_holder_image_still_records_the_root_key() public {
        address root = makeAddr("root signing key");
        bytes memory journal = journalFor(
            HOLDER_IMAGE, holderInstance, keyLeaf(0, address(0), attestation.ROOT_KEY_PURPOSE(), root)
        );
        attestation.setRootKey(sealOf(journal), journal, root, new bytes32[](0));
        assertEq(attestation.rootKey().signer, root);
        assertEq(attestation.rootKey().imageDigest, HOLDER_IMAGE);
        assertTrue(attestation.isRootKeyLive());
        // Recording the root blesses nothing.
        assertTrue(attestation.keyInfo(holderInstance).kind == ImageKind.None);
        assertTrue(attestation.keyInfo(root).kind == ImageKind.None);
    }
}
