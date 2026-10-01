// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// One chain a strategy trades on, by its name in `shonin.toml` ("base",
/// "hyperliquid", ...), with the drivers it trades through there.
struct ChainNeed {
    bytes32 chain;
    bytes32[] drivers;
}

/// One registered strategy. A changed strategy is a new strategy with a new
/// id; nothing here is ever edited.
struct Strategy {
    address author;
    /// The source the builder and the reviewer read.
    bytes32 sourceHash;
    /// The bundle it produced: the component and its models.
    bytes32 bundleHash;
    bytes32 manifestHash;
    uint64 registeredAt;
    /// Every chain the strategy trades on, which its vault's stations are
    /// created from. Nobody chooses them but this record.
    ChainNeed[] chains;
    bytes32[] bridges;
    /// Who its vault refuses deposits from: every driver's list on every
    /// chain, plus the author's additions, sorted and without repeats.
    bytes32[] restricted;
}

/// Every strategy there is, under the id it was given when its author paid.
///
/// An entry needs two attested signatures: the reviewer's go-ahead, given
/// after it read the source and before anything was built, and the builder's,
/// over exactly what it built. So the only bundles here are ones an
/// allowlisted builder compiled from source an allowlisted reviewer read
/// first and did not refuse.
interface IStrategyRegistry {
    event Registered(
        uint256 indexed strategyId,
        address indexed author,
        bytes32 bundleHash,
        bytes32 sourceHash,
        bytes32 manifestHash
    );

    /// Anyone may send this; only the two signatures make it stick.
    function register(
        uint256 strategyId,
        address author,
        bytes32 sourceHash,
        bytes32 bundleHash,
        bytes32 manifestHash,
        ChainNeed[] calldata chains,
        bytes32[] calldata bridges,
        bytes32[] calldata restricted,
        bytes calldata builderSignature,
        bytes calldata reviewerClearance
    ) external;

    function isRegistered(uint256 strategyId) external view returns (bool);
    function strategyOf(uint256 strategyId) external view returns (Strategy memory);
    /// Zero until registered.
    function bundleHashOf(uint256 strategyId) external view returns (bytes32);
    function authorOf(uint256 strategyId) external view returns (address);

    /// What a builder key signs over.
    function registrationDigest(
        uint256 strategyId,
        address author,
        bytes32 sourceHash,
        bytes32 bundleHash,
        bytes32 manifestHash,
        ChainNeed[] calldata chains,
        bytes32[] calldata bridges,
        bytes32[] calldata restricted
    ) external view returns (bytes32);
    /// What a reviewer key signs to let source be built and registered.
    function clearanceDigest(uint256 strategyId, bytes32 sourceHash) external view returns (bytes32);
}
