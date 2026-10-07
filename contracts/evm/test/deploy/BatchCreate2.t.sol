// SPDX-License-Identifier: BUSL-1.1
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";

import {BatchCreate2} from "../../src/deploy/BatchCreate2.sol";

/// Something to create: it remembers what it was given.
contract Holder {
    address public given;

    constructor(address given_) {
        given = given_;
    }
}

/// A creation that always fails.
contract Refuses {
    constructor() {
        revert("no");
    }
}

contract BatchCreate2Test is Test {
    /// The deterministic deployer's code, as it is on every public chain.
    bytes constant DEPLOYER_CODE =
        hex"7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe03601600081602082378035828234f58015156039578182fd5b8082525050506014600cf3";

    BatchCreate2 batch;

    function setUp() public {
        address deployer = 0x4e59b44847b379578588920cA78FbF26c0B4956C;
        if (deployer.code.length == 0) vm.etch(deployer, DEPLOYER_CODE);
        batch = new BatchCreate2();
    }

    function _holder(address given) internal pure returns (bytes memory) {
        return abi.encodePacked(type(Holder).creationCode, abi.encode(given));
    }

    function _alone(bytes32 salt, bytes memory code) internal returns (address at) {
        (bool ok, bytes memory out) = batch.DEPLOYER().call(abi.encodePacked(salt, code));
        require(ok, "the deployer refused");
        at = address(bytes20(out));
    }

    function test_each_contract_lands_where_the_deterministic_deployer_alone_puts_it() public {
        bytes32 salt = bytes32(uint256(4));
        bytes memory first = _holder(address(0xA11CE));
        address firstAt = batch.addressOf(salt, keccak256(first));
        // The second is given the first's address before either exists.
        bytes memory second = _holder(firstAt);

        uint256 snapshot = vm.snapshotState();
        address firstAlone = _alone(salt, first);
        address secondAlone = _alone(salt, second);
        vm.revertToState(snapshot);

        BatchCreate2.Creation[] memory creations = new BatchCreate2.Creation[](2);
        creations[0] = BatchCreate2.Creation(salt, first);
        creations[1] = BatchCreate2.Creation(salt, second);
        vm.prank(address(0xBEEF));
        (address[] memory created, uint256[] memory gasUsed) = batch.create(creations);

        assertEq(created[0], firstAlone);
        assertEq(created[1], secondAlone);
        assertEq(created[0], firstAt);
        assertEq(Holder(created[1]).given(), firstAt);
        assertGt(gasUsed[0], 0);
        assertGt(gasUsed[1], 0);
    }

    function test_a_batch_sent_again_creates_nothing_and_names_the_same_addresses() public {
        BatchCreate2.Creation[] memory creations = new BatchCreate2.Creation[](1);
        creations[0] = BatchCreate2.Creation(bytes32(0), _holder(address(1)));
        (address[] memory once,) = batch.create(creations);
        (address[] memory twice, uint256[] memory gasUsed) = batch.create(creations);
        assertEq(twice[0], once[0]);
        assertEq(gasUsed[0], 0);
    }

    function test_whoever_sends_it_the_addresses_are_the_same() public {
        BatchCreate2.Creation[] memory creations = new BatchCreate2.Creation[](1);
        creations[0] = BatchCreate2.Creation(bytes32(uint256(9)), _holder(address(2)));
        uint256 snapshot = vm.snapshotState();
        vm.prank(address(0x1));
        (address[] memory one,) = batch.create(creations);
        vm.revertToState(snapshot);
        vm.prank(address(0x2));
        (address[] memory two,) = batch.create(creations);
        assertEq(one[0], two[0]);
    }

    function test_a_creation_that_fails_creates_nothing_of_its_batch() public {
        bytes memory fine = _holder(address(3));
        BatchCreate2.Creation[] memory creations = new BatchCreate2.Creation[](2);
        creations[0] = BatchCreate2.Creation(bytes32(0), fine);
        creations[1] = BatchCreate2.Creation(bytes32(0), type(Refuses).creationCode);
        address failing = batch.addressOf(bytes32(0), keccak256(type(Refuses).creationCode));
        vm.expectRevert(abi.encodeWithSelector(BatchCreate2.NotCreated.selector, 1, failing));
        batch.create(creations);
        assertEq(batch.addressOf(bytes32(0), keccak256(fine)).code.length, 0);
    }
}
