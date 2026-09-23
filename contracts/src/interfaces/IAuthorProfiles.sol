// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// What an author says about themselves. Nothing here is verified, and nothing
/// depends on it: the record is what speaks for a strategy.
struct Profile {
    string handle;
    string blurb;
    string[] linkLabels;
    string[] linkUrls;
    uint64 joinedAt;
}

interface IAuthorProfiles {
    event ProfileSet(address indexed author, string handle);

    function setProfile(
        string calldata handle,
        string calldata blurb,
        string[] calldata linkLabels,
        string[] calldata linkUrls
    ) external;

    function profileOf(address author) external view returns (Profile memory);
    function authorOfHandle(string calldata handle) external view returns (address);
}
