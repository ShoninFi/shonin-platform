// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IPublishing, Submission} from "./interfaces/IPublishing.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";

/// Takes the flat fee to publish and hands out strategy ids.
///
/// The fee covers the review and the build, and is spent whatever the outcome:
/// a refusal, a failed build and a job that fails on our side all cost it, so
/// nothing an author can trigger earns a free retry. Ids are assigned here, by
/// the first on-chain act, so they are unique without anyone choosing them.
///
/// The fee goes on to the platform at once. A transfer the dollar refuses
/// (the platform's address blocklisted by the dollar's issuer) neither stops
/// the submission nor loses the fee: it stays here, owed to the platform,
/// and goes with the next fee, or with `collect`, once the dollar takes it.
contract Publishing is IPublishing {
    using SafeERC20 for IERC20;

    IERC20 private immutable _asset;
    address public immutable platform;
    uint256 public immutable fee;

    Submission[] private _submissions;
    /// Fees taken and not yet sent on to the platform.
    uint256 public owed;

    /// The platform given is zero or this contract.
    error BadPlatform();

    /// A platform of zero, which the dollar never pays, or this contract,
    /// which would "pay" itself and keep every fee here, is refused: either
    /// would hold every fee for good.
    constructor(IERC20 asset_, address platform_, uint256 fee_) {
        if (platform_ == address(0) || platform_ == address(this)) revert BadPlatform();
        _asset = asset_;
        platform = platform_;
        fee = fee_;
    }

    function submit(bytes32 upload) external returns (uint256 strategyId) {
        _asset.safeTransferFrom(msg.sender, address(this), fee);
        owed += fee;
        if (_asset.trySafeTransfer(platform, owed)) {
            owed = 0;
        } else {
            emit FeeHeld(owed);
        }
        _submissions.push(Submission({author: msg.sender, upload: upload, paidAt: uint64(block.timestamp)}));
        strategyId = _submissions.length;
        emit Submitted(strategyId, msg.sender, upload, fee);
    }

    /// Anyone: send what is owed on to the platform. Reverts while the
    /// dollar still refuses it.
    function collect() external returns (uint256 amount) {
        amount = owed;
        if (amount == 0) return 0;
        owed = 0;
        _asset.safeTransfer(platform, amount);
        emit Collected(amount);
    }

    function submissionOf(uint256 strategyId) external view returns (Submission memory) {
        if (strategyId == 0 || strategyId > _submissions.length) {
            return Submission({author: address(0), upload: bytes32(0), paidAt: 0});
        }
        return _submissions[strategyId - 1];
    }

    function count() external view returns (uint256) {
        return _submissions.length;
    }

    function asset() external view returns (address) {
        return address(_asset);
    }
}
