// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// A score a reviewer signed. It says whether the code is reasonable, and
/// never what it might earn.
struct Review {
    bytes32 bundleHash;
    bytes32 sourceHash;
    bytes32 reviewerImage;
    /// The pinned model, and the prompt and output schema it was given.
    bytes32 modelIdHash;
    bytes32 promptHash;
    /// The full report, which the page links.
    bytes32 reportHash;
    /// 0 to 100; higher is more reasonable.
    uint8 score;
    uint16 findingCount;
    uint64 reviewedAt;
    /// Who paid for it.
    address boughtBy;
}

/// Every review ever signed, by strategy version. Reviews accumulate: a newer
/// one never erases an older one.
interface IReviewRegistry {
    event ReviewRequested(
        bytes32 indexed strategyId, uint32 indexed version, address indexed by, uint256 fee
    );
    event ReviewSubmitted(
        bytes32 indexed strategyId,
        uint32 indexed version,
        address indexed reviewer,
        uint8 score,
        bytes32 reportHash
    );

    /// Pay for a review of this version. Anyone may buy one at any time.
    function requestReview(bytes32 strategyId, uint32 version) external;
    /// A blessed reviewer key posts what its model returned.
    function submitReview(bytes32 strategyId, uint32 version, Review calldata review) external;

    function reviewCount(bytes32 strategyId, uint32 version) external view returns (uint256);
    function reviewAt(
        bytes32 strategyId,
        uint32 version,
        uint256 index
    ) external view returns (Review memory);
    function latestReview(bytes32 strategyId, uint32 version) external view returns (Review memory);
    function reviewFee() external view returns (uint256);
}
