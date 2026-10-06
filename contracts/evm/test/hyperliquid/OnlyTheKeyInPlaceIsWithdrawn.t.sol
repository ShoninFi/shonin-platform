// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {ImageKind} from "../../src/interfaces/IAttestationRegistry.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// The account has one agent name, and a revoke hands it to an address
/// nobody holds a key for. So a revoke withdraws the agent only when the key
/// named is the one the station has in place and may be revoked; naming any
/// other key withdraws nothing.
contract OnlyTheKeyInPlaceIsWithdrawnOnHyperliquidTest is HyperliquidVault {
    function setUp() public override {
        super.setUp();
        core.credit(address(station), 10e6);
        station.approveKey(HL, executor);
        _nextBlock();
        assertEq(core.agentNamed(address(station), keccak256("shonin")), executor, "the executor trades");
    }

    function test_naming_an_address_nobody_blessed_leaves_the_blessed_executors_agent_in_place() public {
        // Refusing the call is fine; taking the executor's agent away is not.
        address stranger = makeAddr("stranger");
        vm.prank(stranger);
        (bool ok,) = address(station).call(abi.encodeCall(Station.revokeKey, (HL, stranger)));
        ok;
        _nextBlock();

        assertEq(
            core.agentNamed(address(station), keccak256("shonin")),
            executor,
            "naming another key cut off the blessed executor's agent"
        );
        assertEq(station.approvedKey(HL), executor);
    }

    function test_withdrawing_a_replaced_key_leaves_its_successors_agent_in_place() public {
        address fresh = _blessAFreshExecutor();
        vm.prank(fresh);
        station.approveKey(HL, fresh);
        _nextBlock();
        assertEq(core.agentNamed(address(station), keccak256("shonin")), fresh);

        // The old key's revoke, as the keeper sends it after a skipped one.
        vm.prank(bob);
        station.revokeKey(HL, executor);
        _nextBlock();

        assertEq(
            core.agentNamed(address(station), keccak256("shonin")),
            fresh,
            "the successor's agent was withdrawn"
        );
        assertEq(station.approvedKey(HL), fresh);
    }

    function test_a_lapsed_key_in_place_is_withdrawn_by_anyone() public {
        vm.prank(admin);
        attestation.revokeImage(EXECUTOR_IMAGE);

        vm.prank(bob);
        station.revokeKey(HL, executor);
        _nextBlock();

        assertEq(core.agentNamed(address(station), keccak256("shonin")), hl.burnAgent(address(station)));
        assertEq(station.approvedKey(HL), address(0));
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
