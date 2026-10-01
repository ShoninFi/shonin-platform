// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// Restricted lists: short codes such as `US` or `SANCTIONED`, zero-padded,
/// kept sorted and without repeats.
library Codes {
    /// The list as a depositor's statement carries it: comma-separated.
    function text(bytes32[] memory list) internal pure returns (string memory) {
        bytes memory out;
        for (uint256 i = 0; i < list.length; i++) {
            if (i != 0) out = bytes.concat(out, ",");
            bytes32 code = list[i];
            uint256 length;
            while (length < 32 && code[length] != 0) length++;
            bytes memory trimmed = new bytes(length);
            for (uint256 j = 0; j < length; j++) {
                trimmed[j] = code[j];
            }
            out = bytes.concat(out, trimmed);
        }
        return string(out);
    }

    /// `list` with `code` added in order, unless it is already there.
    function with(bytes32[] memory list, bytes32 code) internal pure returns (bytes32[] memory out) {
        uint256 at = list.length;
        for (uint256 i = 0; i < list.length; i++) {
            if (list[i] == code) return list;
            if (list[i] > code) {
                at = i;
                break;
            }
        }
        out = new bytes32[](list.length + 1);
        for (uint256 i = 0; i < at; i++) {
            out[i] = list[i];
        }
        out[at] = code;
        for (uint256 i = at; i < list.length; i++) {
            out[i + 1] = list[i];
        }
    }
}
