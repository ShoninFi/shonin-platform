// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// Values that change at set chain times, for the keeper's differential
/// tests. Each value, named by a key, is a list of steps: "this value from
/// this chain time on". A read gives the newest step at or before the block's
/// time, and zero before the first.
///
/// The test scripts the steps before the keepers start; after that only the
/// keepers' own transactions write, and each write is a step from the block's
/// time on. A write placed before a later scripted step gives way to it when
/// its time comes, so the script still decides what happens next.
abstract contract Scripted {
    struct Step {
        uint64 from;
        uint256 value;
    }

    error NotScripted();
    error NotTheScripter();

    /// Who may script: the account that deployed the contract.
    address internal immutable scripter;

    mapping(bytes32 => Step[]) internal steps;

    constructor() {
        scripter = msg.sender;
    }

    /// The key a value is held under: its name and up to two indices (a
    /// claim's id, a holder's address), packed.
    function key(string memory name, uint256 a, uint256 b) public pure returns (bytes32) {
        return keccak256(abi.encodePacked(name, a, b));
    }

    /// Script many values at once: `values[i]` under `keys[i]` from
    /// `froms[i]` on.
    function script(bytes32[] calldata keys, uint64[] calldata froms, uint256[] calldata values) external {
        if (msg.sender != scripter) revert NotTheScripter();
        for (uint256 i = 0; i < keys.length; i++) {
            _insert(keys[i], froms[i], values[i]);
        }
    }

    /// Every step under a key, for a test that wants to see them.
    function stepsOf(bytes32 k) external view returns (Step[] memory) {
        return steps[k];
    }

    function _at(bytes32 k) internal view returns (uint256) {
        Step[] storage list = steps[k];
        for (uint256 i = list.length; i > 0; i--) {
            if (list[i - 1].from <= block.timestamp) return list[i - 1].value;
        }
        return 0;
    }

    function _get(string memory name) internal view returns (uint256) {
        return _at(key(name, 0, 0));
    }

    function _get(string memory name, uint256 a) internal view returns (uint256) {
        return _at(key(name, a, 0));
    }

    function _get(string memory name, uint256 a, uint256 b) internal view returns (uint256) {
        return _at(key(name, a, b));
    }

    /// A transaction's effect: `value` from this block's time on.
    function _set(string memory name, uint256 value) internal {
        _insert(key(name, 0, 0), uint64(block.timestamp), value);
    }

    function _set(string memory name, uint256 a, uint256 value) internal {
        _insert(key(name, a, 0), uint64(block.timestamp), value);
    }

    function _set(string memory name, uint256 a, uint256 b, uint256 value) internal {
        _insert(key(name, a, b), uint64(block.timestamp), value);
    }

    /// Insert in time order; a step at the same time as earlier ones goes
    /// after them, so the latest write at a moment wins.
    function _insert(bytes32 k, uint64 from, uint256 value) private {
        Step[] storage list = steps[k];
        list.push(Step(from, value));
        uint256 i = list.length - 1;
        while (i > 0 && list[i - 1].from > from) {
            list[i] = list[i - 1];
            i--;
        }
        list[i] = Step(from, value);
    }

    function _addr(uint256 value) internal pure returns (address) {
        return address(uint160(value));
    }

    function _word(address value) internal pure returns (uint256) {
        return uint256(uint160(value));
    }
}
