// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// How one area of the code reads to the reviewer.
enum Rating {
    None,
    LooksFine,
    SomeConcerns,
    SeriousConcerns,
    CouldntTell
}

/// The report card a reviewer signed. It says how the code reads, area by
/// area, and never what it might earn. There is no overall score.
struct Review {
    /// The bundle built from the source; zero in a refusal, where nothing was
    /// built.
    bytes32 bundleHash;
    bytes32 sourceHash;
    bytes32 reviewerImage;
    /// The pinned model, and the prompt and output schema it was given.
    bytes32 modelIdHash;
    bytes32 promptHash;
    /// The full report, which the page links.
    bytes32 reportHash;
    /// One `Rating` per area, in order: security, financial risk,
    /// reliability, reproducibility, outside influence, accurate description.
    uint8[6] ratings;
    uint16 findingCount;
    uint64 reviewedAt;
}

/// One review per strategy, read once, at submission.
///
/// A review is advice with one exception: code found to be malicious is
/// refused before it is built, and the refusal is recorded here against the
/// source it refused. Nobody can buy a fresh review, so nobody can probe a
/// sealed strategy's code by asking again.
interface IReviewRegistry {
    event ReviewSubmitted(
        uint256 indexed strategyId, address indexed reviewer, uint8[6] ratings, bytes32 reportHash
    );
    event Refused(
        uint256 indexed strategyId, address indexed reviewer, bytes32 sourceHash, bytes32 reportHash
    );

    /// A blessed reviewer key posts the review of a registered strategy. The
    /// hashes must be the ones the strategy registry holds.
    function submitReview(uint256 strategyId, Review calldata review) external;
    /// A blessed reviewer key records that it refused a submitted strategy's
    /// source. A refused strategy is never built or registered.
    function submitRefusal(uint256 strategyId, Review calldata review) external;

    /// The review, and whether it was a refusal. Zeros if there is none.
    function reviewOf(uint256 strategyId) external view returns (Review memory review, bool refused);
    function hasReview(uint256 strategyId) external view returns (bool);
}
