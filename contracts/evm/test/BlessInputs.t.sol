// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {StrategyRegistry} from "../src/StrategyRegistry.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// `bless` refuses journal inputs that mean "nothing" elsewhere: a zero
/// signer, and a vault key that names no chain or bundle.
contract BlessInputsTest is Fixture {
    /// `bless` checks only `vault != 0` for an executor or an unwinder. The
    /// chain and the bundle may both be zero, so a key can be blessed for
    /// "vault V on chain 0 running bundle 0", a target that no vault asks
    /// about but that `blessedUntil` and `isBlessedFor` answer for, and that
    /// the `Blessed` event and `keyInfo` report as a real blessing. A service
    /// key is held to all three being zero; a vault key should be held to all
    /// three being set.
    function test_an_executor_is_blessed_for_a_chain_and_a_bundle_not_just_a_vault() public {
        address vault = makeAddr("vault");
        bytes32 leaf =
            keccak256(bytes.concat(keccak256(abi.encode(uint256(0), vault, bytes32(0), block.chainid))));
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, leaf);
        bytes memory seal = sealOf(journal);
        vm.expectRevert(AttestationRegistry.WrongKindForVault.selector);
        attestation.bless(seal, journal, 0, vault, bytes32(0), new bytes32[](0));
    }

    /// `bless` accepts `journal.signer == address(0)`. `StrategyRegistry`'s
    /// `_signer` returns `address(0)` for any malformed signature, so the
    /// moment the zero address is blessed as a builder, `register` accepts an
    /// EMPTY builder signature; blessed as a reviewer, it accepts an empty
    /// clearance. The registry should refuse to bless the zero address, and
    /// `_signer` should revert rather than return the sentinel.
    function test_the_zero_address_cannot_be_blessed_as_a_builder() public {
        bytes memory journal = journalFor(BUILDER_IMAGE, address(0), serviceLeaf());
        bytes memory seal = sealOf(journal);
        vm.expectRevert();
        attestation.bless(seal, journal, 0, address(0), bytes32(0), new bytes32[](0));
    }

    function test_an_empty_signature_is_never_a_builder_signature() public {
        uint256 id = submitAs(author);
        ChainNeed[] memory chains = oneChain();
        bytes32[] memory none = new bytes32[](0);
        bytes memory clearance = signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")));
        vm.expectRevert(StrategyRegistry.BadSignature.selector);
        strategies.register(
            id,
            author,
            keccak256("source"),
            keccak256("bundle"),
            keccak256("manifest"),
            chains,
            none,
            none,
            hex"",
            clearance
        );
        assertFalse(strategies.isRegistered(id), "registered with no builder signature at all");
        assertFalse(attestation.isBlessedAs(address(0), ImageKind.Builder));
    }
}
