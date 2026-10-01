// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "../src/Vault.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// Only an executor key moves capital or charges the tank. An unwinder key
/// blessed for the vault only ever closes.
contract UnwinderKeyTest is Fixture {
    uint256 internal constant UNWINDER_KEY = 0x0A1D;
    bytes32 internal constant UNWINDER_IMAGE = keccak256("unwinder image");

    Vault internal vault;
    bytes32 internal bundle;
    address internal unwinder = vm.addr(UNWINDER_KEY);

    function setUp() public override {
        super.setUp();
        vm.prank(admin);
        attestation.proposeImage(UNWINDER_IMAGE, ImageKind.Unwinder);
        skip(TIMELOCK);
        blessServiceKey(BUILDER_IMAGE, builder);
        blessServiceKey(REVIEWER_IMAGE, reviewer);
        (vault, bundle) = openVault();

        bytes memory journal = journalFor(UNWINDER_IMAGE, unwinder, leafFor(address(vault), bundle));
        attestation.bless(sealOf(journal), journal, block.chainid, address(vault), bundle, new bytes32[](0));
        assertTrue(attestation.keyInfo(unwinder).kind == ImageKind.Unwinder);
    }

    function test_an_unwinder_key_cannot_move_capital_into_the_venue() public {
        depositAs(vault, alice, 1000e6);
        (bytes32 idle, bytes32 venueAccount) = (vault.ACCOUNT_IDLE(), vault.ACCOUNT_VENUE());
        vm.prank(unwinder);
        vm.expectRevert(Vault.NotBlessed.selector);
        vault.moveCapital(idle, venueAccount, 500e6);
    }

    function test_an_unwinder_key_cannot_charge_the_tank() public {
        uint64 day = uint64(block.timestamp / 1 days);
        skip(1 days);
        bytes memory statement = signed(UNWINDER_KEY, vault.usageDigest(day, 2e6));
        vm.expectRevert(Vault.NotBlessed.selector);
        vault.submitUsage(day, 2e6, statement);
    }
}
