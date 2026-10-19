// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IMirrorFeed} from "./IMirrorFeed.sol";

/// A test network's copy of one mainnet Chainlink feed, for a chain that has
/// none of its own: read exactly as the real feed is (`decimals`,
/// `latestRoundData`, `getRoundData`), so a driver needs only its address.
///
/// Each round is the source's own: its round id, its answer and its
/// `updatedAt`, posted by the updater as the source moves. Nothing is
/// invented between the source's rounds, and an older round never replaces
/// a newer one. `startedAt` is the round's `updatedAt` and `answeredInRound`
/// its id, as on a current Chainlink feed.
///
/// Test networks only: it lives with the tests, which no mainnet deploy
/// builds.
contract MirrorFeed is IMirrorFeed {
    struct Round {
        int256 answer;
        uint64 updatedAt;
    }

    uint8 public immutable decimals;
    uint256 public immutable version;
    address public immutable source;
    uint256 public immutable sourceChainId;
    address public updater;
    string public description;

    uint80 private _latest;
    mapping(uint80 => Round) private _rounds;

    /// `roundId`, `answer` and `updatedAt` are the source's latest round, so
    /// the feed answers from its first block.
    constructor(
        address updater_,
        uint8 decimals_,
        string memory description_,
        uint256 version_,
        address source_,
        uint256 sourceChainId_,
        uint80 roundId,
        int256 answer,
        uint256 updatedAt
    ) {
        updater = updater_;
        decimals = decimals_;
        description = description_;
        version = version_;
        source = source_;
        sourceChainId = sourceChainId_;
        emit UpdaterSet(updater_);
        _post(roundId, answer, updatedAt);
    }

    function update(uint80 roundId, int256 answer, uint256 updatedAt) external {
        if (msg.sender != updater) revert NotUpdater();
        _post(roundId, answer, updatedAt);
    }

    function setUpdater(address next) external {
        if (msg.sender != updater) revert NotUpdater();
        updater = next;
        emit UpdaterSet(next);
    }

    function latestRoundData()
        external
        view
        returns (uint80 roundId, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound)
    {
        return _round(_latest);
    }

    function getRoundData(uint80 roundId)
        external
        view
        returns (uint80, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound)
    {
        return _round(roundId);
    }

    function _round(uint80 roundId) private view returns (uint80, int256, uint256, uint256, uint80) {
        Round memory round = _rounds[roundId];
        if (round.updatedAt == 0) revert NoDataPresent();
        return (roundId, round.answer, round.updatedAt, round.updatedAt, roundId);
    }

    function _post(uint80 roundId, int256 answer, uint256 updatedAt) private {
        if (roundId == 0 || updatedAt == 0 || updatedAt > type(uint64).max) revert StaleRound();
        if (_latest != 0) {
            // Never back, never the same round twice, never a round updated
            // before the one it follows.
            if (roundId <= _latest || updatedAt < _rounds[_latest].updatedAt) revert StaleRound();
        }
        _rounds[roundId] = Round({answer: answer, updatedAt: uint64(updatedAt)});
        _latest = roundId;
        emit AnswerUpdated(answer, roundId, updatedAt);
    }
}
