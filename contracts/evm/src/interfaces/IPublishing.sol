// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// One paid submission.
struct Submission {
    /// The wallet that paid. The build service starts the job only if it is
    /// also the wallet the upload named.
    address author;
    /// The reference the build service gave the uploaded archive. Only a
    /// reference: nothing here vouches for what it points at.
    bytes32 upload;
    uint64 paidAt;
}

/// Where publishing starts. Paying the fee from the wallet that uploaded the
/// code admits the job and gives the strategy its id, which every registry
/// then records it under.
interface IPublishing {
    event Submitted(uint256 indexed strategyId, address indexed author, bytes32 indexed upload, uint256 fee);

    /// Pay the fee, in the chain's dollar, and take the next id.
    function submit(bytes32 upload) external returns (uint256 strategyId);

    /// Zeros for an id never assigned.
    function submissionOf(uint256 strategyId) external view returns (Submission memory);
    /// How many ids have been assigned. Ids start at 1.
    function count() external view returns (uint256);
    function fee() external view returns (uint256);
    function asset() external view returns (address);
    function platform() external view returns (address);
}
