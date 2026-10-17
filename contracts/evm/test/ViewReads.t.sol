// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {ViewReads} from "../src/ViewReads.sol";
import {FollowProbe} from "./follow/FollowProbe.sol";
import {MockArbSys} from "./follow/MockArbSys.sol";
import {Test} from "forge-std/Test.sol";

/// Reverts with `size` bytes of data, or spends its gas down to `left` and then reverts
/// with a custom error.
contract Refuser {
    error Spent(uint256 left);

    function loud(uint256 size) external pure {
        assembly {
            revert(0, size)
        }
    }

    function spendThenRefuse(uint256 left) external view {
        while (gasleft() > left) {}
        revert Spent(gasleft());
    }

    function spin() external pure {
        assembly {
            for {} 1 {} {}
        }
    }
}

/// Plays ArbSys at 0x64 for the Orbit branch: its block number is set apart from
/// `block.number`, it answers a distinct hash for any number (so the window is the
/// helper's own), and it can be told to fail.
contract OrbitArbSys {
    error InvalidBlockNumber(uint256 requested, uint256 current);

    uint256 public l2Number;
    uint256 public refusedHash;
    uint256 public shortHash;
    // 0: answers; 1: arbBlockNumber reverts; 2: arbBlockNumber returns 16 bytes.
    uint256 public mode;

    function configure(uint256 number, uint256 refused, uint256 short, uint256 mode_) external {
        l2Number = number;
        refusedHash = refused;
        shortHash = short;
        mode = mode_;
    }

    function arbBlockNumber() external view returns (uint256) {
        if (mode == 1) revert("no number");
        if (mode == 2) {
            assembly {
                return(0, 16)
            }
        }
        return l2Number;
    }

    function arbBlockHash(uint256 n) external view returns (bytes32) {
        if (n == refusedHash) revert InvalidBlockNumber(n, l2Number);
        if (n == shortHash) {
            assembly {
                return(0, 31)
            }
        }
        return l2Hash(n);
    }

    function l2Hash(uint256 n) public pure returns (bytes32) {
        return keccak256(abi.encode("l2", n));
    }
}

contract ViewReadsTest is Test {
    address internal constant ARB_SYS = address(0x64);
    uint256 internal constant MIB = 1_048_576;

    ViewReads internal helper = new ViewReads();
    // Hashes, recent hashes and balances every `attempt` also asks for; none unless a test
    // sets them.
    uint256[] internal attemptHashesOf;
    uint256 internal attemptRecent;
    address[] internal attemptBalancesOf;
    FollowProbe internal probe = new FollowProbe();
    Refuser internal refuser = new Refuser();

    struct Out {
        ViewReads.Context context;
        bytes32[] hashes;
        bytes32[] recent;
        uint256[] balances;
        ViewReads.Result[] results;
    }

    function setUp() external {
        probe.set(7);
        vm.roll(1000);
    }

    // ---- helpers ----

    function item(address target, bytes memory data) internal pure returns (ViewReads.Read memory) {
        return ViewReads.Read(target, data);
    }

    function valueRead() internal view returns (ViewReads.Read memory) {
        return item(address(probe), abi.encodeCall(probe.value, ()));
    }

    function pair(ViewReads.Read memory first) internal view returns (ViewReads.Read[] memory out) {
        out = new ViewReads.Read[](2);
        out[0] = first;
        out[1] = valueRead();
    }

    function repeat(
        ViewReads.Read memory one,
        uint256 n
    ) internal pure returns (ViewReads.Read[] memory out) {
        out = new ViewReads.Read[](n);
        for (uint256 i; i < n; ++i) {
            out[i] = one;
        }
    }

    function items(
        ViewReads.Read[] memory reads,
        uint256 innerGas,
        uint256 resultBytes
    ) internal view returns (ViewReads.Result[] memory results) {
        (,,,, results) =
            helper.read(reads, new uint256[](0), 0, new address[](0), innerGas, resultBytes, MIB, false);
    }

    function blocks(
        uint256[] memory hashesOf,
        uint256 recentHashes,
        bool arbSys
    ) internal view returns (Out memory out) {
        (out.context, out.hashes, out.recent, out.balances, out.results) = helper.read(
            new ViewReads.Read[](0), hashesOf, recentHashes, new address[](0), 100_000, 32, MIB, arbSys
        );
    }

    function attempt(
        ViewReads.Read[] memory reads,
        uint256 innerGas,
        uint256 resultBytes,
        uint256 outer
    ) internal view returns (bool returned, Out memory out) {
        try helper.read{gas: outer}(
            reads, attemptHashesOf, attemptRecent, attemptBalancesOf, innerGas, resultBytes, MIB, false
        ) returns (
            ViewReads.Context memory context,
            bytes32[] memory hashes,
            bytes32[] memory recent,
            uint256[] memory balances,
            ViewReads.Result[] memory results
        ) {
            return (true, Out(context, hashes, recent, balances, results));
        } catch {
            return (false, out);
        }
    }

    function numbers(uint256 a) internal pure returns (uint256[] memory out) {
        out = new uint256[](1);
        out[0] = a;
    }

    function assertOutcome(ViewReads.Result memory result, ViewReads.Outcome outcome) internal pure {
        assertEq(uint256(result.outcome), uint256(outcome));
    }

    function assertValueRead(ViewReads.Result memory result) internal pure {
        assertOutcome(result, ViewReads.Outcome.Returned);
        assertEq(abi.decode(result.data, (uint256)), 7);
    }

    // ---- item outcomes ----

    function testAWriteIsRefusedAndTheNextReadStillWorks() external view {
        ViewReads.Result[] memory results =
            items(pair(item(address(probe), abi.encodeCall(probe.write, ()))), 100_000, 64);
        // A write inside a static call halts and spends all the gas it was given, which the
        // EVM does not tell apart from running out of gas.
        assertOutcome(results[0], ViewReads.Outcome.OutOfGas);
        assertEq(results[0].data.length, 0);
        assertValueRead(results[1]);
        assertEq(probe.value(), 7);
    }

    function testAnOversizedOutputReportsItsSizeAndDoesNotHoldUpTheNext() external view {
        ViewReads.Result[] memory results =
            items(pair(item(address(probe), abi.encodeCall(probe.blob, (5000)))), 100_000, 4096);
        assertOutcome(results[0], ViewReads.Outcome.Oversized);
        // offset, length and 5,000 bytes rounded up to whole words
        assertEq(results[0].data, abi.encode(uint256(64 + 5024)));
        assertValueRead(results[1]);
    }

    function testAnOversizedRevertIsOversizedNotReverted() external view {
        ViewReads.Result[] memory results =
            items(pair(item(address(refuser), abi.encodeCall(refuser.loud, (65)))), 100_000, 64);
        assertOutcome(results[0], ViewReads.Outcome.Oversized);
        assertEq(results[0].data, abi.encode(uint256(65)));
        assertValueRead(results[1]);
    }

    function testAnOutputExactlyAtTheResultLimitIsReturned() external view {
        ViewReads.Result[] memory results =
            items(pair(item(address(probe), abi.encodeCall(probe.blob, (4032)))), 100_000, 4096);
        assertOutcome(results[0], ViewReads.Outcome.Returned);
        assertEq(abi.decode(results[0].data, (bytes)), probe.blob(4032));
        results = items(pair(item(address(probe), abi.encodeCall(probe.blob, (4033)))), 100_000, 4096);
        assertOutcome(results[0], ViewReads.Outcome.Oversized);
    }

    function testAnItemThatRunsOutOfGasIsOutOfGasAndDoesNotHoldUpTheNext() external view {
        ViewReads.Result[] memory results =
            items(pair(item(address(probe), abi.encodeCall(probe.burn, (1_000_000)))), 100_000, 64);
        assertOutcome(results[0], ViewReads.Outcome.OutOfGas);
        assertEq(results[0].data.length, 0);
        assertValueRead(results[1]);

        results = items(pair(item(address(refuser), abi.encodeCall(refuser.spin, ()))), 1, 64);
        assertOutcome(results[0], ViewReads.Outcome.OutOfGas);
        assertOutcome(results[1], ViewReads.Outcome.OutOfGas);
    }

    function testAnItemWithinItsGasReturnsItsValue() external view {
        ViewReads.Result[] memory results =
            items(pair(item(address(probe), abi.encodeCall(probe.burn, (100)))), 100_000, 64);
        assertOutcome(results[0], ViewReads.Outcome.Returned);
        assertEq(abi.decode(results[0].data, (uint256)), 107);
    }

    function testACustomErrorIsReturnedAsReverted() external view {
        ViewReads.Result[] memory results =
            items(pair(item(address(probe), abi.encodeCall(probe.refuse, ()))), 100_000, 64);
        assertOutcome(results[0], ViewReads.Outcome.Reverted);
        assertEq(results[0].data, abi.encodeWithSelector(FollowProbe.Refused.selector, 7));
        assertValueRead(results[1]);
    }

    function testAReasonStringIsReturnedAsReverted() external view {
        ViewReads.Result[] memory results =
            items(pair(item(address(probe), abi.encodeCall(probe.refuseWithReason, ()))), 100_000, 128);
        assertOutcome(results[0], ViewReads.Outcome.Reverted);
        assertEq(results[0].data, abi.encodeWithSignature("Error(string)", "refused"));
        assertValueRead(results[1]);
    }

    function testAnEmptyRevertIsRevertedWithNoData() external view {
        ViewReads.Result[] memory results =
            items(pair(item(address(probe), abi.encodeCall(probe.refuseEmpty, ()))), 100_000, 64);
        assertOutcome(results[0], ViewReads.Outcome.Reverted);
        assertEq(results[0].data.length, 0);
        assertValueRead(results[1]);
    }

    function testARevertWithDataNearTheGasLimitIsRevertedNotOutOfGas() external view {
        ViewReads.Result[] memory results =
            items(pair(item(address(refuser), abi.encodeCall(refuser.spendThenRefuse, (300)))), 100_000, 64);
        assertOutcome(results[0], ViewReads.Outcome.Reverted);
        assertEq(bytes4(results[0].data), Refuser.Spent.selector);
        assertValueRead(results[1]);
    }

    function testAnItemAtAnAddressWithoutCodeReturnsNoData() external view {
        ViewReads.Result[] memory results = items(pair(item(address(0xBEEF), hex"12345678")), 100_000, 64);
        assertOutcome(results[0], ViewReads.Outcome.Returned);
        assertEq(results[0].data.length, 0);
        assertValueRead(results[1]);
    }

    // ---- bounds ----

    function testReadsAreAcceptedUpTo512AndRefusedAt513() external {
        ViewReads.Result[] memory results = items(repeat(valueRead(), 512), 100_000, 32);
        assertEq(results.length, 512);
        for (uint256 i; i < results.length; ++i) {
            assertValueRead(results[i]);
        }
        vm.expectRevert(ViewReads.Bounds.selector);
        items(repeat(valueRead(), 513), 100_000, 32);
    }

    function testHashesOfAreAcceptedUpTo256AndRefusedAt257() external {
        Out memory out = blocks(new uint256[](256), 0, false);
        assertEq(out.hashes.length, 256);
        vm.expectRevert(ViewReads.Bounds.selector);
        blocks(new uint256[](257), 0, false);
    }

    function testRecentHashesAreAcceptedUpTo256AndRefusedAt257() external {
        Out memory out = blocks(new uint256[](0), 256, false);
        assertEq(out.recent.length, 256);
        vm.expectRevert(ViewReads.Bounds.selector);
        blocks(new uint256[](0), 257, false);
    }

    function testBalancesOfAreAcceptedUpTo256AndRefusedAt257() external {
        (,,, uint256[] memory balances,) = helper.read(
            new ViewReads.Read[](0), new uint256[](0), 0, new address[](256), 100_000, 32, MIB, false
        );
        assertEq(balances.length, 256);
        vm.expectRevert(ViewReads.Bounds.selector);
        helper.read(new ViewReads.Read[](0), new uint256[](0), 0, new address[](257), 100_000, 32, MIB, false);
    }

    function testInnerGasIsAcceptedFrom1To1MillionAndRefusedOutside() external {
        assertValueRead(items(pair(valueRead()), 1_000_000, 32)[0]);
        items(pair(valueRead()), 1, 32);
        vm.expectRevert(ViewReads.Bounds.selector);
        items(pair(valueRead()), 0, 32);
        vm.expectRevert(ViewReads.Bounds.selector);
        items(pair(valueRead()), 1_000_001, 32);
    }

    function testResultBytesAreAcceptedFrom32To16384AndRefusedOutside() external {
        assertValueRead(items(pair(valueRead()), 100_000, 32)[0]);
        assertValueRead(items(pair(valueRead()), 100_000, 16_384)[0]);
        vm.expectRevert(ViewReads.Bounds.selector);
        items(pair(valueRead()), 100_000, 31);
        vm.expectRevert(ViewReads.Bounds.selector);
        items(pair(valueRead()), 100_000, 16_385);
    }

    function testInputBytesMustCoverTheCalldataAndStayWithin1MiB() external {
        ViewReads.Read[] memory reads = repeat(valueRead(), 3);
        uint256[] memory none = new uint256[](0);
        address[] memory nobody = new address[](0);
        uint256 length = abi.encodeCall(helper.read, (reads, none, 0, nobody, 100_000, 32, 0, false)).length;
        (,,,, ViewReads.Result[] memory results) =
            helper.read(reads, none, 0, nobody, 100_000, 32, length, false);
        assertEq(results.length, 3);
        vm.expectRevert(ViewReads.Bounds.selector);
        helper.read(reads, none, 0, nobody, 100_000, 32, length - 1, false);
        vm.expectRevert(ViewReads.Bounds.selector);
        helper.read(reads, none, 0, nobody, 100_000, 32, MIB + 1, false);
    }

    function testTheOutputBoundIsAcceptedAt1MiBAndRefusedOnePast() external {
        // 640 fixed + 32 * 44 hashes + 64 items of 160 + 16,192: exactly 1 MiB.
        assertEq(helper.outputBound(64, 44, 0, 0, 16_170), MIB);
        assertEq(helper.outputBound(64, 20, 12, 12, 16_192), MIB);
        assertEq(helper.outputBound(64, 45, 0, 0, 16_161), MIB + 32);
        ViewReads.Read[] memory reads = repeat(valueRead(), 64);
        (,,,, ViewReads.Result[] memory results) =
            helper.read(reads, new uint256[](20), 12, new address[](12), 100_000, 16_170, MIB, false);
        assertEq(results.length, 64);
        vm.expectRevert(ViewReads.Bounds.selector);
        helper.read(reads, new uint256[](21), 12, new address[](12), 100_000, 16_170, MIB, false);
        vm.expectRevert(ViewReads.Bounds.selector);
        helper.read(reads, new uint256[](20), 12, new address[](12), 100_000, 16_193, MIB, false);
    }

    function testTheOutputBoundCoversTheEncodedOutput(
        uint8 count,
        uint16 size,
        uint8 hashCount
    ) external view {
        uint256 n = bound(count, 0, 40);
        uint256 resultBytes = bound(size, 64, 4096);
        // Every item returns the most data it may: a blob whose encoding is resultBytes
        // rounded down to whole words.
        uint256 blobLength = (resultBytes / 32) * 32 - 64;
        ViewReads.Read[] memory reads =
            repeat(item(address(probe), abi.encodeCall(probe.blob, (blobLength))), n);
        uint256[] memory hashesOf = new uint256[](bound(hashCount, 0, 256));
        address[] memory balancesOf = new address[](3);
        (
            ViewReads.Context memory context,
            bytes32[] memory hashes,
            bytes32[] memory recent,
            uint256[] memory balances,
            ViewReads.Result[] memory results
        ) = helper.read(reads, hashesOf, 5, balancesOf, 100_000, resultBytes, MIB, false);
        for (uint256 i; i < n; ++i) {
            assertOutcome(results[i], ViewReads.Outcome.Returned);
        }
        uint256 encoded = abi.encode(context, hashes, recent, balances, results).length;
        assertLe(encoded, helper.outputBound(n, hashesOf.length, 5, 3, resultBytes));
    }

    // ---- outer gas ----

    /// The gas the helper keeps back for its output, as it computes it.
    function reserveFor(
        uint256 reads,
        uint256 hashes,
        uint256 recent,
        uint256 balances,
        uint256 resultBytes
    ) internal view returns (uint256) {
        uint256 words = (2 * helper.outputBound(reads, hashes, recent, balances, resultBytes)) / 32 + 2048;
        return 3 * words + words * words / 512 + 50_000 + 200 * reads + 100 * (hashes + recent + balances);
    }

    function testLowOuterGasLeavesTheTailNotRunAndStillReturns() external view {
        ViewReads.Read[] memory reads = repeat(item(address(probe), abi.encodeCall(probe.burn, (300))), 20);
        uint256 innerGas = 100_000;
        uint256 floor = innerGas * 64 / 63 + 10_000 + reserveFor(20, 0, 0, 0, 64);
        uint256 outer = floor + 400_000;
        (bool returned, Out memory out) = attempt(reads, innerGas, 64, outer);
        assertTrue(returned);
        assertLe(out.context.gasAtEntry, outer);
        assertEq(out.results.length, 20);
        uint256 ran;
        while (ran < 20 && out.results[ran].outcome != ViewReads.Outcome.NotRun) {
            assertOutcome(out.results[ran], ViewReads.Outcome.Returned);
            assertEq(abi.decode(out.results[ran].data, (uint256)), 307);
            ++ran;
        }
        assertGt(ran, 0);
        assertLt(ran, 20);
        for (uint256 i = ran; i < 20; ++i) {
            assertOutcome(out.results[i], ViewReads.Outcome.NotRun);
            assertEq(out.results[i].data.length, 0);
        }
    }

    function testGasAtEntryIsAtMostTheGasGiven() external view {
        (bool returned, Out memory out) = attempt(new ViewReads.Read[](0), 100_000, 32, 2_000_000);
        assertTrue(returned);
        assertLe(out.context.gasAtEntry, 2_000_000);
        assertGt(out.context.gasAtEntry, 1_900_000);
    }

    /// Whether the call returns at this outer gas, and how many items ran before the first
    /// NotRun. Every account the call touches starts cold, as in a fresh `eth_call`, so
    /// earlier attempts do not make later ones cheaper. Gives back the memory the attempt
    /// used, so many attempts in one test stay cheap.
    function ranAt(
        ViewReads.Read[] memory reads,
        uint256 innerGas,
        uint256 resultBytes,
        uint256 outer
    ) internal returns (bool returned, uint256 ran) {
        vm.cool(address(helper));
        for (uint256 i; i < reads.length; ++i) {
            vm.cool(reads[i].target);
        }
        for (uint256 i; i < attemptBalancesOf.length; ++i) {
            vm.cool(attemptBalancesOf[i]);
        }
        uint256 mark;
        assembly ("memory-safe") {
            mark := mload(0x40)
        }
        Out memory out;
        (returned, out) = attempt(reads, innerGas, resultBytes, outer);
        if (returned) {
            while (ran < out.results.length && out.results[ran].outcome != ViewReads.Outcome.NotRun) {
                ++ran;
            }
        }
        assembly ("memory-safe") {
            mstore(0x40, mark)
        }
    }

    /// Smallest outer gas in [low, high] at which the call either runs more than `count`
    /// items or fails.
    function threshold(
        ViewReads.Read[] memory reads,
        uint256 innerGas,
        uint256 resultBytes,
        uint256 count,
        uint256 low,
        uint256 high
    ) internal returns (uint256) {
        while (low < high) {
            uint256 mid = (low + high) / 2;
            (bool returned, uint256 ran) = ranAt(reads, innerGas, resultBytes, mid);
            if (!returned || ran > count) high = mid;
            else low = mid + 1;
        }
        return low;
    }

    /// Within `span` of the outer gas at which the first item and the last item start to
    /// run, and at `steps` steps between them, the call always returns: the reserve covers everything after the
    /// last gas check.
    function assertReturnsAroundEveryThreshold(
        ViewReads.Read[] memory reads,
        uint256 innerGas,
        uint256 resultBytes,
        uint256 span,
        uint256 steps
    ) internal {
        uint256 n = reads.length;
        uint256 floor = innerGas * 64 / 63 + 10_000
            + reserveFor(n, attemptHashesOf.length, attemptRecent, attemptBalancesOf.length, resultBytes);
        // Find an outer gas at which the call returns before the first item: enough for the
        // work before the items and the encoding, short of the first item's gas check. (Less
        // than that is too little for any answer, and the call runs out of gas.)
        uint256 low = floor;
        (bool returned, uint256 ran) = ranAt(reads, innerGas, resultBytes, low);
        while (!returned) {
            low += 10_000;
            (returned, ran) = ranAt(reads, innerGas, resultBytes, low);
        }
        assertEq(ran, 0);
        uint256 first = threshold(reads, innerGas, resultBytes, 0, low, 60_000_000);
        uint256 last = threshold(reads, innerGas, resultBytes, n - 1, first, 60_000_000);
        assertLt(first, last);
        for (uint256 g = first - span; g <= first + span; ++g) {
            (returned, ran) = ranAt(reads, innerGas, resultBytes, g);
            assertTrue(returned);
            assertEq(ran, g < first ? 0 : 1);
        }
        for (uint256 g = last - span; g <= last + span; ++g) {
            (returned, ran) = ranAt(reads, innerGas, resultBytes, g);
            assertTrue(returned);
            assertEq(ran, g < last ? n - 1 : n);
        }
        for (uint256 g = first; g <= last; g += (last - first) / steps + 1) {
            (returned,) = ranAt(reads, innerGas, resultBytes, g);
            assertTrue(returned);
        }
    }

    function testAtEveryOuterGasPast512SmallItemsTheCallReturns() external {
        assertReturnsAroundEveryThreshold(repeat(valueRead(), 512), 5000, 32, 40, 50);
    }

    function testAtEveryOuterGasPast512SmallItemsAndEveryHashAndBalanceTheCallReturns() external {
        for (uint256 i; i < 256; ++i) {
            attemptHashesOf.push(900 + i);
            attemptBalancesOf.push(address(uint160(0x1000 + i)));
            // An account that exists, so each attempt can make it cold again.
            vm.deal(attemptBalancesOf[i], 1);
        }
        attemptRecent = 256;
        assertReturnsAroundEveryThreshold(repeat(valueRead(), 512), 5000, 32, 20, 20);
    }

    function testAtEveryOuterGasPastTheLargestOutputsTheCallReturns() external {
        // 63 items each returning 16,384 bytes: just under the 1 MiB output bound.
        assertReturnsAroundEveryThreshold(
            repeat(item(address(probe), abi.encodeCall(probe.blob, (16_384 - 64))), 63),
            1_000_000,
            16_384,
            4,
            3
        );
    }

    // ---- mixed outcomes ----

    function kindRead(uint256 kind) internal view returns (ViewReads.Read memory) {
        if (kind == 0) return valueRead();
        if (kind == 1) return item(address(probe), abi.encodeCall(probe.refuse, ()));
        if (kind == 2) return item(address(probe), abi.encodeCall(probe.refuseEmpty, ()));
        if (kind == 3) return item(address(probe), abi.encodeCall(probe.burn, (1_000_000)));
        if (kind == 4) return item(address(probe), abi.encodeCall(probe.blob, (500)));
        if (kind == 5) return item(address(probe), abi.encodeCall(probe.write, ()));
        return item(address(uint160(0xBEEF)), "");
    }

    function kindOutcome(uint256 kind) internal pure returns (ViewReads.Outcome) {
        if (kind == 0 || kind == 6) return ViewReads.Outcome.Returned;
        if (kind == 1 || kind == 2) return ViewReads.Outcome.Reverted;
        if (kind == 4) return ViewReads.Outcome.Oversized;
        return ViewReads.Outcome.OutOfGas;
    }

    function testAnyMixOfOutcomesReturnsOneResultPerItemAndNotRunOnlyAsATail(
        uint256 seed,
        uint32 gasSeed
    ) external view {
        uint256 n = seed % 41;
        uint256[] memory kinds = new uint256[](n);
        ViewReads.Read[] memory reads = new ViewReads.Read[](n);
        for (uint256 i; i < n; ++i) {
            kinds[i] = uint256(keccak256(abi.encode(seed, i))) % 7;
            reads[i] = kindRead(kinds[i]);
        }
        uint256 innerGas = 50_000;
        uint256 floor = innerGas * 64 / 63 + 10_000 + reserveFor(n, 0, 0, 0, 256);
        uint256 outer = bound(gasSeed, floor + 200_000, floor + 2_500_000);
        (bool returned, Out memory out) = attempt(reads, innerGas, 256, outer);
        assertTrue(returned);
        assertEq(out.results.length, n);
        bool stopped;
        for (uint256 i; i < n; ++i) {
            if (out.results[i].outcome == ViewReads.Outcome.NotRun) stopped = true;
            // An item that runs gets its full gas, so its outcome is the one it has with
            // plenty of outer gas.
            if (stopped) assertOutcome(out.results[i], ViewReads.Outcome.NotRun);
            else assertOutcome(out.results[i], kindOutcome(kinds[i]));
        }
    }

    // ---- block context, plain branch ----

    function testThePlainBranchReportsTheBlockItRunsAt() external {
        vm.roll(5000);
        vm.warp(1_700_000_000);
        Out memory out = blocks(new uint256[](0), 0, false);
        assertEq(out.context.number, 5000);
        assertEq(out.context.timestamp, 1_700_000_000);
        assertEq(out.context.parentHash, blockhash(4999));
        assertTrue(out.context.parentHash != bytes32(0));
    }

    function testThePlainBranchAnswersHashesOnlyInsideThe256BlockWindow() external {
        vm.roll(5000);
        uint256[] memory hashesOf = new uint256[](7);
        hashesOf[0] = 4999;
        hashesOf[1] = 4744; // number - 256
        hashesOf[2] = 4900;
        hashesOf[3] = 4743; // number - 257
        hashesOf[4] = 5000;
        hashesOf[5] = 5001;
        hashesOf[6] = 0;
        Out memory out = blocks(hashesOf, 256, false);
        assertEq(out.hashes[0], blockhash(4999));
        assertEq(out.hashes[1], blockhash(4744));
        assertEq(out.hashes[2], blockhash(4900));
        assertTrue(out.hashes[0] != bytes32(0) && out.hashes[1] != bytes32(0));
        for (uint256 i = 3; i < 7; ++i) {
            assertEq(out.hashes[i], bytes32(0));
        }
        for (uint256 i; i < 256; ++i) {
            assertEq(out.recent[i], blockhash(4999 - i));
            assertTrue(out.recent[i] != bytes32(0));
        }
    }

    function testRecentHashesBelowBlockZeroAreZero() external {
        vm.roll(2);
        Out memory out = blocks(numbers(0), 5, false);
        assertEq(out.recent[0], blockhash(1));
        assertEq(out.recent[1], blockhash(0));
        assertEq(out.hashes[0], blockhash(0));
        for (uint256 i = 2; i < 5; ++i) {
            assertEq(out.recent[i], bytes32(0));
        }
    }

    function testAtBlockZeroTheParentHashIsZero() external {
        vm.roll(0);
        Out memory out = blocks(numbers(0), 3, false);
        assertEq(out.context.number, 0);
        assertEq(out.context.parentHash, bytes32(0));
        assertEq(out.hashes[0], bytes32(0));
        assertEq(out.recent[2], bytes32(0));
    }

    function testBalancesAreTheNativeBalances() external {
        address[] memory who = new address[](3);
        who[0] = address(0xA11CE);
        who[1] = address(0xB0B);
        who[2] = address(0xC0FFEE);
        vm.deal(who[0], 5 ether);
        vm.deal(who[1], 1);
        (,,, uint256[] memory balances,) =
            helper.read(new ViewReads.Read[](0), new uint256[](0), 0, who, 100_000, 32, MIB, false);
        assertEq(balances[0], 5 ether);
        assertEq(balances[1], 1);
        assertEq(balances[2], 0);
    }

    // ---- block context, ArbSys branch ----

    function orbit(
        uint256 l2Number,
        uint256 refused,
        uint256 short,
        uint256 mode
    ) internal returns (OrbitArbSys) {
        vm.etch(ARB_SYS, address(new OrbitArbSys()).code);
        OrbitArbSys(ARB_SYS).configure(l2Number, refused, short, mode);
        return OrbitArbSys(ARB_SYS);
    }

    function testTheArbSysBranchTakesNumberAndHashesOnlyFromArbSys() external {
        // block.number plays the parent chain's number, far below the chain's own.
        vm.roll(1000);
        vm.warp(1_700_000_123);
        OrbitArbSys arb = orbit(50_000, type(uint256).max, type(uint256).max, 0);
        uint256[] memory hashesOf = new uint256[](8);
        hashesOf[0] = 49_999;
        hashesOf[1] = 49_744; // number - 256
        hashesOf[2] = 49_743; // number - 257
        hashesOf[3] = 50_000;
        hashesOf[4] = 50_001;
        hashesOf[5] = 999; // the parent chain's previous block
        hashesOf[6] = 0;
        hashesOf[7] = 49_900;
        Out memory out = blocks(hashesOf, 3, true);
        assertEq(out.context.number, 50_000);
        assertEq(out.context.timestamp, 1_700_000_123);
        assertEq(out.context.parentHash, arb.l2Hash(49_999));
        assertEq(out.hashes[0], arb.l2Hash(49_999));
        assertEq(out.hashes[1], arb.l2Hash(49_744));
        assertEq(out.hashes[7], arb.l2Hash(49_900));
        for (uint256 i = 2; i < 7; ++i) {
            assertEq(out.hashes[i], bytes32(0));
        }
        for (uint256 i; i < 3; ++i) {
            assertEq(out.recent[i], arb.l2Hash(49_999 - i));
        }
    }

    function testAHashArbSysRefusesOrAnswersShortIsZeroForThatItemOnly() external {
        OrbitArbSys arb = orbit(50_000, 49_990, 49_980, 0);
        uint256[] memory hashesOf = new uint256[](4);
        hashesOf[0] = 49_990;
        hashesOf[1] = 49_991;
        hashesOf[2] = 49_980;
        hashesOf[3] = 49_981;
        Out memory out = blocks(hashesOf, 0, true);
        assertEq(out.hashes[0], bytes32(0));
        assertEq(out.hashes[1], arb.l2Hash(49_991));
        assertEq(out.hashes[2], bytes32(0));
        assertEq(out.hashes[3], arb.l2Hash(49_981));
        assertEq(out.context.parentHash, arb.l2Hash(49_999));
    }

    function testTheArbSysBranchRevertsWhenArbSysHasNoCode() external {
        vm.etch(ARB_SYS, "");
        vm.expectRevert(ViewReads.NoBlockNumber.selector);
        blocks(new uint256[](0), 0, true);
    }

    function testTheArbSysBranchRevertsWhenTheBlockNumberCallFails() external {
        orbit(50_000, 0, 0, 1);
        vm.expectRevert(ViewReads.NoBlockNumber.selector);
        blocks(new uint256[](0), 0, true);
    }

    function testTheArbSysBranchRevertsWhenTheBlockNumberIsShort() external {
        orbit(50_000, 0, 0, 2);
        vm.expectRevert(ViewReads.NoBlockNumber.selector);
        blocks(new uint256[](0), 0, true);
    }

    function testTheLocalArbSysStandInAgreesWithThePlainBranch() external {
        vm.roll(5000);
        vm.etch(ARB_SYS, address(new MockArbSys()).code);
        uint256[] memory hashesOf = new uint256[](5);
        hashesOf[0] = 4999;
        hashesOf[1] = 4744;
        hashesOf[2] = 4743;
        hashesOf[3] = 5000;
        hashesOf[4] = 1;
        Out memory plain = blocks(hashesOf, 256, false);
        Out memory arb = blocks(hashesOf, 256, true);
        assertEq(arb.context.number, plain.context.number);
        assertEq(arb.context.parentHash, plain.context.parentHash);
        assertEq(abi.encode(arb.hashes), abi.encode(plain.hashes));
        assertEq(abi.encode(arb.recent), abi.encode(plain.recent));
    }
}
