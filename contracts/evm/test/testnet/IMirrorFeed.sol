// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// Chainlink's price feed interface, as its proxies answer it.
interface AggregatorV3Interface {
    function decimals() external view returns (uint8);
    function description() external view returns (string memory);
    function version() external view returns (uint256);
    function getRoundData(uint80 roundId)
        external
        view
        returns (uint80 roundId_, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound);
    function latestRoundData()
        external
        view
        returns (uint80 roundId, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound);
}

/// A test network's copy of one mainnet Chainlink feed: the same answers,
/// decimals and round ids, posted by one updater. Each round names the
/// mainnet round it copies, so anyone can read that round on the source feed
/// (`source` on chain `sourceChainId`) and compare.
interface IMirrorFeed is AggregatorV3Interface {
    /// The mainnet feed this one copies, and its chain.
    function source() external view returns (address);
    function sourceChainId() external view returns (uint256);
    /// The one account that may post a round.
    function updater() external view returns (address);

    /// Post the source's round `roundId`: its answer and when it was
    /// updated. Refused from anyone but the updater, for a round no newer
    /// than the latest, or one updated before it.
    function update(uint80 roundId, int256 answer, uint256 updatedAt) external;
    /// Hand posting to another account.
    function setUpdater(address next) external;

    /// Chainlink's own event for a new answer.
    event AnswerUpdated(int256 indexed current, uint256 indexed roundId, uint256 updatedAt);
    event UpdaterSet(address indexed updater);

    error NotUpdater();
    error StaleRound();
    error NoDataPresent();
}
