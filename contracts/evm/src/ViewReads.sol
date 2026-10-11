// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// Code used only by read-only RPC state overrides, never deployed.
/// Callers must restrict destinations and selectors before using this helper.
contract ViewReads {
    struct Read {
        address target;
        bytes data;
    }

    struct Result {
        bool success;
        bytes data;
    }

    error Bounds();

    function read(
        Read[] calldata reads,
        uint256 innerGas,
        uint256 resultBytes,
        uint256 inputBytes
    )
        external
        view
        returns (uint256 number, uint256 timestamp, bytes32 parentHash, Result[] memory results)
    {
        if (
            reads.length > 64 || innerGas == 0 || innerGas > 1_000_000 || resultBytes < 32
                || resultBytes > 16_384 || inputBytes > 1_048_576 || msg.data.length > inputBytes
        ) revert Bounds();
        results = new Result[](reads.length);
        for (uint256 i; i < reads.length; ++i) {
            bytes memory input = reads[i].data;
            address target = reads[i].target;
            bool success;
            bytes memory output;
            assembly ("memory-safe") {
                success := staticcall(innerGas, target, add(input, 32), mload(input), 0, 0)
                let size := returndatasize()
                if gt(size, resultBytes) {
                    success := 0
                    size := 0
                }
                output := mload(0x40)
                mstore(output, size)
                returndatacopy(add(output, 32), 0, size)
                mstore(0x40, and(add(add(output, size), 63), not(31)))
            }
            results[i] = Result(success, output);
        }
        return (block.number, block.timestamp, blockhash(block.number - 1), results);
    }
}
