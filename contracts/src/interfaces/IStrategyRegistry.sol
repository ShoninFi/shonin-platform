// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

enum VersionStatus {
    None,
    Registered,
    Retired
}

/// One published strategy version. A new version is a new entry and a new
/// vault; nothing here is ever edited.
struct StrategyVersion {
    address author;
    /// The source the builder read.
    bytes32 sourceHash;
    /// The bundle it produced: the component and its models.
    bytes32 bundleHash;
    bytes32 manifestHash;
    /// The version this one claims to replace. A claim, not a mechanism.
    bytes32 supersedesId;
    uint32 supersedesVersion;
    uint64 registeredAt;
    VersionStatus status;
}

/// Every strategy version there is, and the builder's signature that put it
/// there. An entry without a valid builder signature is refused, so the only
/// bundles on the registry are ones an allowlisted builder compiled.
interface IStrategyRegistry {
    event Registered(
        bytes32 indexed strategyId,
        uint32 indexed version,
        address indexed author,
        bytes32 bundleHash,
        bytes32 sourceHash,
        bytes32 manifestHash
    );
    event Retired(bytes32 indexed strategyId, uint32 indexed version);

    /// Anyone may send this; only a signature from a blessed builder key makes
    /// it stick.
    function register(
        bytes32 strategyId,
        uint32 version,
        address author,
        bytes32 sourceHash,
        bytes32 bundleHash,
        bytes32 manifestHash,
        bytes32 supersedesId,
        uint32 supersedesVersion,
        bytes calldata builderSignature
    ) external;

    /// The author stops their own strategy. The record stays.
    function retire(bytes32 strategyId, uint32 version) external;

    function versionOf(bytes32 strategyId, uint32 version) external view returns (StrategyVersion memory);
    function bundleHashOf(bytes32 strategyId, uint32 version) external view returns (bytes32);
    /// What a builder key signs over.
    function registrationDigest(
        bytes32 strategyId,
        uint32 version,
        address author,
        bytes32 sourceHash,
        bytes32 bundleHash,
        bytes32 manifestHash
    ) external view returns (bytes32);
}
