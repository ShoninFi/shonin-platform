// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {ImageKind} from "../../src/interfaces/IAttestationRegistry.sol";
import {LighterVault} from "./LighterVault.sol";

/// The accounts have one executor slot, and a revoke puts a key nobody holds
/// in it and cancels the slot's orders. So a revoke does that only when the
/// key named is the one the station has in place and may be revoked; naming
/// any other key withdraws nothing and cancels nothing.
contract OnlyTheKeyInPlaceIsWithdrawnOnLighterTest is LighterVault {
    bytes internal constant FRESH_PUBKEY =
        hex"1a000000000000001b000000000000001c000000000000001d000000000000001e00000000000000";

    function setUp() public override {
        super.setUp();
        _fundAndTrade();
        assertEq(exchange.pubKey(_master(), lighter.KEY_SLOT()), EXECUTOR_PUBKEY, "the executor trades");
    }

    function test_naming_an_address_nobody_blessed_leaves_the_blessed_executors_key_in_its_slot() public {
        uint256 cancelsBefore = exchange.cancels();

        // Refusing the call is fine; taking the executor's key out is not.
        address stranger = makeAddr("stranger");
        vm.prank(stranger);
        (bool ok,) = address(station).call(abi.encodeCall(Station.revokeKey, (LIGHTER, stranger)));
        ok;

        assertEq(
            exchange.pubKey(_master(), lighter.KEY_SLOT()),
            EXECUTOR_PUBKEY,
            "naming another key took the blessed executor's key out of the account"
        );
        assertEq(exchange.cancels(), cancelsBefore, "and cancelled its resting orders");
        assertEq(station.approvedKey(LIGHTER), executor);
    }

    function test_withdrawing_a_replaced_key_leaves_its_successors_key_in_its_slot() public {
        address fresh = _blessAFreshExecutor();
        vm.startPrank(fresh);
        board.setPubKey(FRESH_PUBKEY);
        station.approveKey(LIGHTER, fresh);
        vm.stopPrank();
        assertEq(exchange.pubKey(_master(), lighter.KEY_SLOT()), FRESH_PUBKEY);
        uint256 cancelsBefore = exchange.cancels();

        // The old key's revoke, as the keeper sends it after a skipped one.
        vm.prank(bob);
        station.revokeKey(LIGHTER, executor);

        assertEq(
            exchange.pubKey(_master(), lighter.KEY_SLOT()), FRESH_PUBKEY, "the successor's key was withdrawn"
        );
        assertEq(exchange.cancels(), cancelsBefore, "and its orders cancelled");
        assertEq(station.approvedKey(LIGHTER), fresh);
    }

    function test_a_lapsed_key_in_place_is_withdrawn_by_anyone() public {
        uint256 cancelsBefore = exchange.cancels();
        vm.prank(admin);
        attestation.revokeImage(EXECUTOR_IMAGE);

        vm.prank(bob);
        station.revokeKey(LIGHTER, executor);

        assertEq(exchange.pubKey(_master(), lighter.KEY_SLOT()), lighter.NOBODY());
        assertGt(exchange.cancels(), cancelsBefore, "its orders are cancelled");
        assertEq(station.approvedKey(LIGHTER), address(0));
    }

    /// The executor's image is revoked and a new one's key is blessed.
    function _blessAFreshExecutor() internal returns (address fresh) {
        vm.prank(admin);
        attestation.revokeImage(EXECUTOR_IMAGE);
        bytes32 freshImage = keccak256("fresh executor image");
        vm.prank(admin);
        attestation.proposeImage(freshImage, ImageKind.Executor);
        skip(TIMELOCK);
        fresh = vm.addr(0xF4E5);
        bytes32 bundle = station.spec().bundleHash;
        bytes memory journal = journalFor(freshImage, fresh, leafFor(address(vault), bundle));
        attestation.bless(sealOf(journal), journal, block.chainid, address(vault), bundle, new bytes32[](0));
    }
}
