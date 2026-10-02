// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";

/// Calling a contract as anyone, and checking why it refused.
abstract contract Calls is Test {
    /// The selector of `Panic(uint256)`, which Solidity reverts with on an
    /// index out of range or an arithmetic overflow.
    bytes4 internal constant PANIC = 0x4e487b71;

    /// Call `target` with `data` as `caller`. Returns whether it went through
    /// and, if not, the selector it reverted with.
    function callAs(
        address caller,
        address target,
        bytes memory data
    ) internal returns (bool ok, bytes4 err) {
        bytes memory answer;
        vm.prank(caller);
        (ok, answer) = target.call(data);
        if (!ok && answer.length >= 4) err = bytes4(answer);
    }

    /// It went through when `want` is zero, and was refused with exactly
    /// `want` otherwise.
    function expectOutcome(bool ok, bytes4 err, bytes4 want, string memory what) internal {
        if (want == bytes4(0)) {
            if (!ok) emit log_named_bytes32("refused with", bytes32(err));
            assertTrue(ok, string.concat(what, ": refused where the rules allow it"));
        } else {
            assertFalse(ok, string.concat(what, ": went through where the rules refuse it"));
            assertEq(err, want, string.concat(what, ": refused for the wrong reason"));
        }
    }

    /// A refusal for one of the reasons the function gives, and no other.
    function refusedFor(bool ok, bytes4 err, bytes4[] memory allowed, string memory what) internal {
        if (ok) return;
        for (uint256 i = 0; i < allowed.length; i++) {
            if (err == allowed[i]) return;
        }
        emit log_named_bytes32("refused with", bytes32(err));
        assertTrue(false, string.concat(what, ": refused for a reason it never gives"));
    }

    function errs(bytes4 a) internal pure returns (bytes4[] memory list) {
        list = new bytes4[](1);
        list[0] = a;
    }

    function errs(bytes4 a, bytes4 b) internal pure returns (bytes4[] memory list) {
        list = new bytes4[](2);
        list[0] = a;
        list[1] = b;
    }

    function errs(bytes4 a, bytes4 b, bytes4 c) internal pure returns (bytes4[] memory list) {
        list = new bytes4[](3);
        list[0] = a;
        list[1] = b;
        list[2] = c;
    }

    function errs(bytes4 a, bytes4 b, bytes4 c, bytes4 d) internal pure returns (bytes4[] memory list) {
        list = new bytes4[](4);
        list[0] = a;
        list[1] = b;
        list[2] = c;
        list[3] = d;
    }

    function errs(
        bytes4 a,
        bytes4 b,
        bytes4 c,
        bytes4 d,
        bytes4 e
    ) internal pure returns (bytes4[] memory list) {
        list = new bytes4[](5);
        list[0] = a;
        list[1] = b;
        list[2] = c;
        list[3] = d;
        list[4] = e;
    }

    function errs(
        bytes4 a,
        bytes4 b,
        bytes4 c,
        bytes4 d,
        bytes4 e,
        bytes4 f
    ) internal pure returns (bytes4[] memory list) {
        list = new bytes4[](6);
        list[0] = a;
        list[1] = b;
        list[2] = c;
        list[3] = d;
        list[4] = e;
        list[5] = f;
    }

    function nobody() internal pure returns (address[] memory) {
        return new address[](0);
    }

    function only(address a) internal pure returns (address[] memory list) {
        list = new address[](1);
        list[0] = a;
    }

    function only(address a, address b) internal pure returns (address[] memory list) {
        list = new address[](2);
        list[0] = a;
        list[1] = b;
    }

    function only(address a, address b, address c) internal pure returns (address[] memory list) {
        list = new address[](3);
        list[0] = a;
        list[1] = b;
        list[2] = c;
    }

    function only(address a, address b, address c, address d) internal pure returns (address[] memory list) {
        list = new address[](4);
        list[0] = a;
        list[1] = b;
        list[2] = c;
        list[3] = d;
    }

    function contains(address[] memory list, address who) internal pure returns (bool) {
        for (uint256 i = 0; i < list.length; i++) {
            if (list[i] == who) return true;
        }
        return false;
    }

    /// The first `n` entries of `list` include `who`.
    function seenBefore(address[] memory list, uint256 n, address who) internal pure returns (bool) {
        for (uint256 i = 0; i < n; i++) {
            if (list[i] == who) return true;
        }
        return false;
    }
}
