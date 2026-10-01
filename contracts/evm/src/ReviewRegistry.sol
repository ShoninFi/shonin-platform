// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationRegistry, ImageKind} from "./interfaces/IAttestationRegistry.sol";
import {IPublishing} from "./interfaces/IPublishing.sol";
import {IReviewRegistry, Rating, Review} from "./interfaces/IReviewRegistry.sol";
import {IStrategyRegistry} from "./interfaces/IStrategyRegistry.sol";

/// Every strategy's one review.
///
/// A review is a model's reading of code, signed by the image that ran it. It
/// is never a gate for anything but malicious code, which the reviewer refuses
/// before it is built. Everything else is shown, however it reads, and the
/// depositor decides.
contract ReviewRegistry is IReviewRegistry {
    IAttestationRegistry public immutable attestation;
    IStrategyRegistry public immutable strategies;
    IPublishing public immutable publishing;

    mapping(uint256 => Review) private _review;
    mapping(uint256 => bool) private _refused;

    error NotTheReviewer();
    error NotRegistered();
    error AlreadyRegistered();
    error NotSubmitted();
    error WrongBundle();
    error AlreadyReviewed();
    error BadRating();

    constructor(IAttestationRegistry attestation_, IStrategyRegistry strategies_, IPublishing publishing_) {
        attestation = attestation_;
        strategies = strategies_;
        publishing = publishing_;
    }

    function submitReview(uint256 strategyId, Review calldata review) external {
        _check(strategyId, review);
        if (!strategies.isRegistered(strategyId)) revert NotRegistered();
        // A review is of one bundle built from one source; saying otherwise
        // would let a good report follow code nobody read.
        if (
            strategies.bundleHashOf(strategyId) != review.bundleHash
                || strategies.strategyOf(strategyId).sourceHash != review.sourceHash
        ) revert WrongBundle();

        _record(strategyId, review);
        emit ReviewSubmitted(strategyId, msg.sender, review.ratings, review.reportHash);
    }

    function submitRefusal(uint256 strategyId, Review calldata review) external {
        _check(strategyId, review);
        if (publishing.submissionOf(strategyId).author == address(0)) revert NotSubmitted();
        if (strategies.isRegistered(strategyId)) revert AlreadyRegistered();
        if (review.bundleHash != bytes32(0)) revert WrongBundle();

        _record(strategyId, review);
        _refused[strategyId] = true;
        emit Refused(strategyId, msg.sender, review.sourceHash, review.reportHash);
    }

    /// The image and the time are the registry's own knowledge, not the
    /// reviewer's claim.
    function _record(uint256 strategyId, Review calldata review) private {
        Review storage stored = _review[strategyId];
        stored.bundleHash = review.bundleHash;
        stored.sourceHash = review.sourceHash;
        stored.reviewerImage = attestation.keyInfo(msg.sender).imageDigest;
        stored.modelIdHash = review.modelIdHash;
        stored.promptHash = review.promptHash;
        stored.reportHash = review.reportHash;
        stored.ratings = review.ratings;
        stored.findingCount = review.findingCount;
        stored.reviewedAt = uint64(block.timestamp);
    }

    function _check(uint256 strategyId, Review calldata review) private view {
        if (!attestation.isBlessedAs(msg.sender, ImageKind.Reviewer)) revert NotTheReviewer();
        // One review per strategy, ever.
        if (_review[strategyId].reviewedAt != 0) revert AlreadyReviewed();
        for (uint256 i = 0; i < 6; i++) {
            if (review.ratings[i] == uint8(Rating.None) || review.ratings[i] > uint8(Rating.CouldntTell)) {
                revert BadRating();
            }
        }
    }

    function reviewOf(uint256 strategyId) external view returns (Review memory review, bool refused) {
        return (_review[strategyId], _refused[strategyId]);
    }

    function hasReview(uint256 strategyId) external view returns (bool) {
        return _review[strategyId].reviewedAt != 0 && !_refused[strategyId];
    }
}
