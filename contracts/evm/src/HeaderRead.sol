// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// Read-only RPC override code; never deployed and never writes state.
contract HeaderRead {
    function read(uint256 ancestorNumber)
        external view
        returns (uint256 number, uint256 timestamp, bytes32 parentHash, bytes32 ancestorHash)
    {
        return (block.number, block.timestamp, blockhash(block.number - 1), blockhash(ancestorNumber));
    }
}
