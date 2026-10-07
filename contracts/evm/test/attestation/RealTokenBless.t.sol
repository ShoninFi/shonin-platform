// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../../src/AttestationRegistry.sol";
import {DevAttestationVerifier} from "../../src/DevAttestationVerifier.sol";
import {ConfidentialSpaceVerifier} from "../../src/attestation/ConfidentialSpaceVerifier.sol";
import {ImageKind, KeyInfo} from "../../src/interfaces/IAttestationRegistry.sol";
import {RealTokenBlessFixture as R} from "./RealTokenBlessFixture.sol";
import {Test, console} from "forge-std/Test.sol";
import {ControlID} from "risc0/groth16/ControlID.sol";
import {RiscZeroGroth16Verifier} from "risc0/groth16/RiscZeroGroth16Verifier.sol";

/// The registry blessing keys through the Confidential Space checker with a
/// real Groth16 proof over a genuine token, signed by Google, whose nonce
/// binds an instance key and a root over three leaves that each name a key:
/// two vaults' keys on this chain and a builder's key (see the fixture).
contract RealTokenBlessTest is Test {
    ConfidentialSpaceVerifier checker;
    AttestationRegistry registry;
    address admin = makeAddr("admin");

    uint256 constant VAULT_A = 0;
    uint256 constant VAULT_B = 1;
    uint256 constant BUILDER = 2;

    function setUp() public {
        RiscZeroGroth16Verifier riscZero =
            new RiscZeroGroth16Verifier(ControlID.CONTROL_ROOT, ControlID.BN254_CONTROL_ID);
        checker = new ConfidentialSpaceVerifier(riscZero, R.IMAGE_ID, R.googleKeys());
        DevAttestationVerifier dev = new DevAttestationVerifier(makeAddr("attestor"));
        vm.startPrank(admin);
        registry = new AttestationRegistry(admin, dev, 0, 0, 72 hours);
        registry.proposeVerifier(address(checker));
        registry.proposeImage(R.IMAGE_DIGEST, ImageKind.Executor);
        registry.proposeProject(R.PROJECT_ID);
        vm.stopPrank();
        vm.chainId(31_337);
        vm.warp(R.ISSUED_AT + 600);
    }

    function bless(uint256 i) internal {
        (uint256 vaultChain, address vault, bytes32 strategyHash, address key, bytes32[] memory proof) =
            R.leaf(i);
        registry.bless(R.SEAL, R.JOURNAL, vaultChain, vault, strategyHash, key, proof);
    }

    function test_one_real_seal_blesses_each_vault_s_own_key_for_its_vault_alone() public {
        uint256 before = gasleft();
        bless(VAULT_A);
        console.log("registry.bless gas, a real seal and a two-node proof", before - gasleft());
        bless(VAULT_B);

        (uint256 chainA, address vaultA, bytes32 strategyA, address keyA,) = R.leaf(VAULT_A);
        (uint256 chainB, address vaultB, bytes32 strategyB, address keyB,) = R.leaf(VAULT_B);
        assertTrue(registry.isBlessedFor(keyA, chainA, vaultA, strategyA));
        assertTrue(registry.isBlessedFor(keyB, chainB, vaultB, strategyB));
        // Each key for its own vault only.
        assertFalse(registry.isBlessedFor(keyA, chainB, vaultB, strategyB));
        assertFalse(registry.isBlessedFor(keyB, chainA, vaultA, strategyA));

        KeyInfo memory info = registry.keyInfo(keyA);
        assertEq(info.verifier, address(checker));
        assertEq(info.trustRoot, bytes32("gcp-cs-tdx"));
        assertEq(info.imageDigest, R.IMAGE_DIGEST);
        assertEq(info.projectId, keccak256("shonin-testnet"));
        assertEq(info.blessedUntil, R.ISSUED_AT + 72 hours);
    }

    function test_the_instance_key_the_journal_names_is_not_blessed_by_blessing_a_leaf() public {
        bless(VAULT_A);
        (uint256 vaultChain, address vault, bytes32 strategyHash,,) = R.leaf(VAULT_A);
        assertFalse(registry.isBlessedFor(R.SIGNER, vaultChain, vault, strategyHash));
        assertEq(uint8(registry.keyInfo(R.SIGNER).kind), uint8(ImageKind.None));
    }

    function test_a_leaf_s_proof_with_another_key_or_vault_is_not_among_the_vaults() public {
        (uint256 vaultChain, address vault, bytes32 strategyHash, address key, bytes32[] memory proof) =
            R.leaf(VAULT_A);
        (,,, address otherKey,) = R.leaf(VAULT_B);
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        registry.bless(R.SEAL, R.JOURNAL, vaultChain, vault, strategyHash, otherKey, proof);
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        registry.bless(R.SEAL, R.JOURNAL, vaultChain, makeAddr("another vault"), strategyHash, key, proof);
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        registry.bless(R.SEAL, R.JOURNAL, vaultChain, vault, keccak256("another strategy"), key, proof);
        vm.expectRevert(AttestationRegistry.NoSigner.selector);
        registry.bless(R.SEAL, R.JOURNAL, vaultChain, vault, strategyHash, address(0), proof);
    }

    function test_a_leaf_is_blessed_only_on_the_chain_it_names() public {
        vm.chainId(8453);
        vm.expectRevert(AttestationRegistry.NotAmongTheVaults.selector);
        bless(VAULT_A);
    }

    function test_the_builder_s_leaf_does_not_bless_through_an_executor_image() public {
        vm.expectRevert(AttestationRegistry.WrongKindForVault.selector);
        bless(BUILDER);
    }

    function test_a_tampered_journal_blesses_nothing() public {
        (uint256 vaultChain, address vault, bytes32 strategyHash, address key, bytes32[] memory proof) =
            R.leaf(VAULT_A);
        bytes memory journal = bytes.concat(R.JOURNAL);
        // The vaults root's last byte.
        journal[127] = bytes1(uint8(journal[127]) ^ 0x01);
        vm.expectRevert(AttestationRegistry.ProofRejected.selector);
        registry.bless(R.SEAL, journal, vaultChain, vault, strategyHash, key, proof);
    }

    function test_the_seal_blesses_nothing_once_the_blessing_ttl_has_passed() public {
        vm.warp(R.ISSUED_AT + 72 hours);
        vm.expectRevert(AttestationRegistry.Stale.selector);
        bless(VAULT_A);
    }
}
