// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// Code used only by read-only RPC state overrides, never deployed.
/// One call reads the block it runs at, chosen block hashes, native balances and a
/// list of bounded static calls, so every value in the answer comes from one block.
/// Callers must restrict destinations and selectors before using this helper.
contract ViewReads {
    struct Read {
        address target;
        bytes data;
    }

    enum Outcome {
        Returned,
        Reverted,
        Oversized,
        OutOfGas,
        NotRun
    }

    struct Result {
        Outcome outcome;
        bytes data;
    }

    struct Context {
        uint256 number;
        uint256 timestamp;
        bytes32 parentHash;
        uint256 gasAtEntry;
    }

    error Bounds();
    error NoBlockNumber();

    /// ArbSys, the Arbitrum precompile that knows the chain's own block numbers and hashes.
    /// On an Orbit chain `block.number` is the parent chain's number and `blockhash` of a
    /// chain block is zero, so that branch reads both from here instead.
    address internal constant ARB_SYS = address(0x64);
    /// Gas given to each ArbSys call; the precompile needs far less.
    uint256 internal constant ARB_SYS_GAS = 100_000;
    /// arbBlockNumber() and arbBlockHash(uint256).
    bytes4 internal constant ARB_BLOCK_NUMBER = 0xa3b1b31d;
    bytes4 internal constant ARB_BLOCK_HASH = 0x2b407a82;

    /// Upper bound, in bytes, on the ABI encoding of `read`'s output for these sizes: a fixed
    /// head, one length word plus one word per hash, recent hash and balance, a length word for
    /// the results, and per item an offset, the result head, a length word and the data
    /// rounded up to whole words.
    function outputBound(
        uint256 reads,
        uint256 hashes,
        uint256 recent,
        uint256 balances,
        uint256 resultBytes
    ) public pure returns (uint256) {
        return 32 * 16 + 32 * (hashes + 1) + 32 * (recent + 1) + 32 * (balances + 1) + 32
            + reads * (160 + (resultBytes + 31) / 32 * 32);
    }

    /// Reads the context, the hashes of `hashesOf`, the `recentHashes` hashes before the
    /// current block, the balances of `balancesOf`, then each item as a static call with
    /// `innerGas` gas. `arbSys` selects where block numbers and hashes come from.
    function read(
        Read[] calldata reads,
        uint256[] calldata hashesOf,
        uint256 recentHashes,
        address[] calldata balancesOf,
        uint256 innerGas,
        uint256 resultBytes,
        uint256 inputBytes,
        bool arbSys
    )
        external
        view
        returns (
            Context memory context,
            bytes32[] memory hashes,
            bytes32[] memory recent,
            uint256[] memory balances,
            Result[] memory results
        )
    {
        // First, so an endpoint that silently clamps the call's gas is seen doing so.
        context.gasAtEntry = gasleft();
        if (
            reads.length > 512 || hashesOf.length > 256 || recentHashes > 256 || balancesOf.length > 256
                || innerGas == 0 || innerGas > 1_000_000 || resultBytes < 32 || resultBytes > 16_384
                || inputBytes > 1_048_576 || msg.data.length > inputBytes
        ) revert Bounds();
        // Checked after the sizes, so it cannot overflow.
        uint256 outputSize =
            outputBound(reads.length, hashesOf.length, recentHashes, balancesOf.length, resultBytes);
        if (outputSize > 1_048_576) revert Bounds();

        uint256 number = arbSys ? arbBlockNumber() : block.number;
        context.number = number;
        context.timestamp = block.timestamp;
        context.parentHash = number == 0 ? bytes32(0) : hashOf(number - 1, number, arbSys);

        hashes = new bytes32[](hashesOf.length);
        for (uint256 i; i < hashesOf.length; ++i) {
            hashes[i] = hashOf(hashesOf[i], number, arbSys);
        }
        // recent[i] is the hash of block number - 1 - i; zero below block zero.
        recent = new bytes32[](recentHashes);
        for (uint256 i; i < recentHashes && i < number; ++i) {
            recent[i] = hashOf(number - 1 - i, number, arbSys);
        }
        balances = new uint256[](balancesOf.length);
        for (uint256 i; i < balancesOf.length; ++i) {
            balances[i] = balancesOf[i].balance;
        }

        // Every item starts as NotRun, so stopping early leaves the rest marked without
        // spending gas on them after the last gas check.
        results = new Result[](reads.length);
        for (uint256 i; i < reads.length; ++i) {
            results[i].outcome = Outcome.NotRun;
        }
        // Gas kept back for building and ABI-encoding the output: the cost of memory twice
        // the output bound plus 64 KiB, 50,000 more, and the encoder's own work, 200 for
        // each item and 100 for each hash, recent hash and balance.
        uint256 words = (2 * outputSize) / 32 + 2048;
        uint256 reserve = 3 * words + words * words / 512 + 50_000 + 200 * reads.length
            + 100 * (hashesOf.length + recentHashes + balancesOf.length);
        // An item runs only with gas for its own call (innerGas plus the 1/64 the EVM keeps
        // back), 10,000 for the call's own costs, and the reserve; otherwise it and every
        // later item stay NotRun and the call still returns.
        uint256 floor = innerGas * 64 / 63 + 10_000 + reserve;

        for (uint256 i; i < reads.length; ++i) {
            bytes calldata input = reads[i].data;
            address target = reads[i].target;
            bool run;
            bool success;
            uint256 used;
            assembly ("memory-safe") {
                // The input goes in unallocated memory past the free pointer; the output
                // below overwrites it. Copying it first means its cost is already paid
                // when the gas is checked.
                let at := mload(0x40)
                calldatacopy(at, input.offset, input.length)
                run := iszero(lt(gas(), floor))
                if run {
                    // Warming the target first keeps the account access cost out of `used`,
                    // so `used` is the callee's gas plus about a hundred.
                    pop(extcodesize(target))
                    let before := gas()
                    // A static call: a target that tries to write state halts, spending all
                    // its gas, so it is reported OutOfGas and changes nothing.
                    success := staticcall(innerGas, target, at, input.length, 0, 0)
                    used := sub(before, gas())
                }
            }
            if (!run) break;

            uint256 size;
            assembly ("memory-safe") {
                size := returndatasize()
            }
            Result memory result = results[i];
            if (size > resultBytes) {
                // Too large to copy, whatever the call's success: report only the size.
                result.outcome = Outcome.Oversized;
                result.data = abi.encode(size);
            } else if (!success && size == 0 && used >= innerGas) {
                // Spent all its gas without returning anything. A revert with data is never
                // counted here, even near the limit.
                result.outcome = Outcome.OutOfGas;
            } else {
                result.outcome = success ? Outcome.Returned : Outcome.Reverted;
                bytes memory output;
                assembly ("memory-safe") {
                    output := mload(0x40)
                    mstore(output, size)
                    returndatacopy(add(output, 32), 0, size)
                    mstore(0x40, and(add(add(output, size), 63), not(31)))
                }
                result.data = output;
            }
        }
    }

    /// The chain's own block number from ArbSys; reverts NoBlockNumber if it cannot be read.
    function arbBlockNumber() internal view returns (uint256 chainNumber) {
        bool ok;
        address arb = ARB_SYS;
        assembly ("memory-safe") {
            mstore(0x00, ARB_BLOCK_NUMBER)
            ok := staticcall(ARB_SYS_GAS, arb, 0x00, 4, 0x00, 32)
            ok := and(ok, iszero(lt(returndatasize(), 32)))
            chainNumber := mload(0x00)
        }
        if (!ok) revert NoBlockNumber();
    }

    /// Hash of block `m` as seen from block `number`: zero unless `m` is one of the 256
    /// blocks before `number`. On the ArbSys branch a failed or short answer is zero for
    /// this hash only.
    function hashOf(uint256 m, uint256 number, bool arbSys) internal view returns (bytes32 hash) {
        if (m >= number || number - m > 256) return bytes32(0);
        if (!arbSys) return blockhash(m);
        bool ok;
        address arb = ARB_SYS;
        assembly ("memory-safe") {
            mstore(0x00, ARB_BLOCK_HASH)
            mstore(0x04, m)
            ok := staticcall(ARB_SYS_GAS, arb, 0x00, 36, 0x00, 32)
            ok := and(ok, iszero(lt(returndatasize(), 32)))
            hash := mload(0x00)
        }
        if (!ok) hash = bytes32(0);
    }
}
