// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// Stands in, in tests only, for the canonical Multicall3 deployment: the same function
/// and struct names and the same `aggregate3` semantics, for a local chain without it.
contract StandInMulticall3 {
    struct Call3 {
        address target;
        bool allowFailure;
        bytes callData;
    }

    struct Result {
        bool success;
        bytes returnData;
    }

    /// Calls each target in order; a failed call reverts the whole batch unless it allows
    /// failure.
    function aggregate3(Call3[] calldata calls) public payable returns (Result[] memory returnData) {
        returnData = new Result[](calls.length);
        for (uint256 i; i < calls.length; ++i) {
            Call3 calldata item = calls[i];
            Result memory result = returnData[i];
            (result.success, result.returnData) = item.target.call(item.callData);
            require(item.allowFailure || result.success, "Multicall3: call 3 failed");
        }
    }

    function getBlockNumber() public view returns (uint256 blockNumber) {
        blockNumber = block.number;
    }

    function getCurrentBlockTimestamp() public view returns (uint256 timestamp) {
        timestamp = block.timestamp;
    }

    function getLastBlockHash() public view returns (bytes32 blockHash) {
        blockHash = blockhash(block.number - 1);
    }

    function getBlockHash(uint256 blockNumber) public view returns (bytes32 blockHash) {
        blockHash = blockhash(blockNumber);
    }

    function getEthBalance(address addr) public view returns (uint256 balance) {
        balance = addr.balance;
    }

    function getChainId() public view returns (uint256 chainid) {
        chainid = block.chainid;
    }
}
