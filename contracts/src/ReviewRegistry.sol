// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationRegistry, ImageKind} from "./interfaces/IAttestationRegistry.sol";
import {IReviewRegistry, Review} from "./interfaces/IReviewRegistry.sol";
import {IStrategyRegistry, VersionStatus} from "./interfaces/IStrategyRegistry.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";

/// Every review ever signed, by strategy version.
///
/// A review is one model's opinion of whether code is reasonable, signed by
/// the image that ran it. It is never a gate: nothing here can stop a vault
/// opening or a depositor depositing. Reviews accumulate, because a newer
/// opinion should not be able to bury an older one -- anyone can buy a fresh
/// review, and both stay on the page.
contract ReviewRegistry is IReviewRegistry {
    using SafeERC20 for IERC20;

    IAttestationRegistry public immutable attestation;
    IStrategyRegistry public immutable strategies;
    IERC20 public immutable asset;
    address public immutable platform;
    uint256 public immutable reviewFee;

    mapping(bytes32 => mapping(uint32 => Review[])) private _reviews;

    error NotTheReviewer();
    error NoSuchVersion();
    error WrongBundle();
    error NoReviewYet();

    constructor(
        IAttestationRegistry attestation_,
        IStrategyRegistry strategies_,
        IERC20 asset_,
        address platform_,
        uint256 reviewFee_
    ) {
        attestation = attestation_;
        strategies = strategies_;
        asset = asset_;
        platform = platform_;
        reviewFee = reviewFee_;
    }

    function requestReview(bytes32 strategyId, uint32 version) external {
        asset.safeTransferFrom(msg.sender, platform, reviewFee);
        emit ReviewRequested(strategyId, version, msg.sender, reviewFee);
    }

    function submitReview(bytes32 strategyId, uint32 version, Review calldata review) external {
        if (!attestation.isBlessedAs(msg.sender, ImageKind.Reviewer)) revert NotTheReviewer();
        if (strategies.versionOf(strategyId, version).status == VersionStatus.None) revert NoSuchVersion();
        // A review is of one bundle built from one source; saying otherwise
        // would let a good score follow code nobody scored.
        if (
            strategies.versionOf(strategyId, version).bundleHash != review.bundleHash
                || strategies.versionOf(strategyId, version).sourceHash != review.sourceHash
        ) revert WrongBundle();

        _reviews[strategyId][version].push(review);
        emit ReviewSubmitted(strategyId, version, msg.sender, review.score, review.reportHash);
    }

    function reviewCount(bytes32 strategyId, uint32 version) external view returns (uint256) {
        return _reviews[strategyId][version].length;
    }

    function reviewAt(
        bytes32 strategyId,
        uint32 version,
        uint256 index
    ) external view returns (Review memory) {
        return _reviews[strategyId][version][index];
    }

    function latestReview(bytes32 strategyId, uint32 version) external view returns (Review memory) {
        Review[] storage all = _reviews[strategyId][version];
        if (all.length == 0) revert NoReviewYet();
        return all[all.length - 1];
    }
}
