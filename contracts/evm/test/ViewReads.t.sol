// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {ViewReads} from "../src/ViewReads.sol";
import {Test} from "forge-std/Test.sol";

contract StorageViewProbe {
    uint256 public value = 7;

    function write() external {
        value = 8;
    }

    function large() external pure returns (bytes memory) {
        return new bytes(1024);
    }

    function burn() external pure {
        assembly {
            for {} 1 {} {}
        }
    }
}

contract ViewReadsTest is Test {
    ViewReads internal helper = new ViewReads();
    StorageViewProbe internal probe = new StorageViewProbe();

    function reads(bytes memory input) internal view returns (ViewReads.Read[] memory out) {
        out = new ViewReads.Read[](2);
        out[0] = ViewReads.Read(address(probe), input);
        out[1] = ViewReads.Read(address(probe), abi.encodeCall(probe.value, ()));
    }

    function testStorageWriteIsRefusedAndTheNextReadStillWorks() external view {
        (uint256 number, uint256 timestamp,, ViewReads.Result[] memory results) =
            helper.read(reads(abi.encodeCall(probe.write, ())), 100_000, 64, 4096);
        assertEq(number, block.number);
        assertEq(timestamp, block.timestamp);
        assertFalse(results[0].success);
        assertTrue(results[1].success);
        assertEq(abi.decode(results[1].data, (uint256)), 7);
        assertEq(probe.value(), 7);
    }

    function testOversizedOutputDoesNotHoldUpTheNextRead() external view {
        (,,, ViewReads.Result[] memory results) =
            helper.read(reads(abi.encodeCall(probe.large, ())), 100_000, 64, 4096);
        assertFalse(results[0].success);
        assertEq(results[0].data.length, 0);
        assertTrue(results[1].success);
        assertEq(abi.decode(results[1].data, (uint256)), 7);
    }

    function testInnerGasExhaustionDoesNotHoldUpTheNextRead() external view {
        (,,, ViewReads.Result[] memory results) =
            helper.read(reads(abi.encodeCall(probe.burn, ())), 10_000, 64, 4096);
        assertFalse(results[0].success);
        assertTrue(results[1].success);
    }

    function testItemAndInputBoundsAreEnforced() external {
        ViewReads.Read[] memory items = new ViewReads.Read[](64);
        for (uint256 i; i < items.length; ++i) {
            items[i] = ViewReads.Read(address(probe), abi.encodeCall(probe.value, ()));
        }
        (,,, ViewReads.Result[] memory results) = helper.read(items, 100_000, 64, 65_536);
        assertEq(results.length, 64);
        for (uint256 i; i < results.length; ++i) {
            assertTrue(results[i].success);
        }
        vm.expectRevert(ViewReads.Bounds.selector);
        helper.read(new ViewReads.Read[](65), 100_000, 64, 65_536);
        vm.expectRevert(ViewReads.Bounds.selector);
        helper.read(items, 100_000, 64, 100);
        vm.expectRevert(ViewReads.Bounds.selector);
        helper.read(items, 100_000, 16_385, 65_536);
    }
}
