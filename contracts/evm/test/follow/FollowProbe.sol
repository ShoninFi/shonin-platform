// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// A target for the chain follower's integration tests: state to change, events to
/// emit, and views that return each shape, size and failure the follower must handle.
contract FollowProbe {
    struct Pair {
        uint256 a;
        address b;
        bytes32 c;
    }

    event Noted(uint256 indexed index, uint256 value);

    error Refused(uint256 code);

    uint256 public value;
    mapping(address => uint256) public credit;

    function set(uint256 next) external {
        value = next;
    }

    function give(address to, uint256 amount) external {
        credit[to] += amount;
    }

    /// Emits `count` Noted events, numbered from zero, each carrying the current value.
    function note(uint256 count) external {
        for (uint256 i; i < count; ++i) {
            emit Noted(i, value);
        }
    }

    function pair() external view returns (Pair memory) {
        return Pair(value, address(this), keccak256(abi.encode(value)));
    }

    /// The value, whether it is odd, and the hash of its encoding.
    function triple() external view returns (uint256, bool, bytes32) {
        return (value, value % 2 == 1, keccak256(abi.encode(value)));
    }

    function name() external pure returns (string memory) {
        return "follow probe";
    }

    /// The numbers 0 to n - 1.
    function list(uint256 n) external pure returns (uint256[] memory out) {
        out = new uint256[](n);
        for (uint256 i; i < n; ++i) {
            out[i] = i;
        }
    }

    /// n bytes; byte i is i modulo 256. Written a word at a time, so a large blob costs
    /// little gas: word k holds bytes 32k to 32k + 31, which is the first word's bytes plus
    /// 32 * (k mod 8) in every byte.
    function blob(uint256 n) external pure returns (bytes memory out) {
        out = new bytes(n);
        assembly ("memory-safe") {
            let data := add(out, 32)
            let ones := 0x0101010101010101010101010101010101010101010101010101010101010101
            let first := 0x000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f
            for { let k := 0 } lt(mul(k, 32), n) { k := add(k, 1) } {
                mstore(add(data, mul(k, 32)), add(first, mul(mul(32, mod(k, 8)), ones)))
            }
            // The bytes past n stay zero, as the encoding requires.
            mstore(add(data, n), 0)
        }
    }

    function refuse() external pure {
        revert Refused(7);
    }

    function refuseWithReason() external pure {
        require(false, "refused");
    }

    function refuseEmpty() external pure {
        revert();
    }

    /// Spends gas in proportion to `rounds` (one hash per round, in scratch memory, so the
    /// cost per round is constant) and returns value + rounds.
    function burn(uint256 rounds) external view returns (uint256) {
        bytes32 h;
        for (uint256 i; i < rounds; ++i) {
            assembly ("memory-safe") {
                mstore(0x00, h)
                mstore(0x20, i)
                h := keccak256(0x00, 0x40)
            }
        }
        // Uses the hash, so the loop is kept, without changing the result.
        if (h == bytes32(uint256(1))) return 0;
        return value + rounds;
    }

    function context() external view returns (uint256 number, uint256 timestamp) {
        return (block.number, block.timestamp);
    }

    function write() external returns (uint256) {
        value += 1;
        return value;
    }
}
