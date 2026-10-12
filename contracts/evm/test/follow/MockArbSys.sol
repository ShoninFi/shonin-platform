// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// Answers the two ArbSys block reads the way the precompile at 0x64 does, from the local
/// chain's own blocks, so a local node can play an Arbitrum chain. Tests only.
contract MockArbSys {
    error InvalidBlockNumber(uint256 requested, uint256 current);

    function arbBlockNumber() external view returns (uint256) {
        return block.number;
    }

    /// The hash of one of the 256 blocks before the current one; any other number reverts,
    /// as the precompile does.
    function arbBlockHash(uint256 n) external view returns (bytes32) {
        if (n >= block.number || block.number - n > 256) revert InvalidBlockNumber(n, block.number);
        return blockhash(n);
    }
}
