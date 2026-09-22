// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAuthorProfiles, Profile} from "./interfaces/IAuthorProfiles.sol";

/// What an author says about themselves.
///
/// Nothing here is checked and nothing depends on it. An author who sets no
/// profile is shown by their address, and their record reads exactly the same,
/// because the record is what speaks for a strategy.
contract AuthorProfiles is IAuthorProfiles {
    mapping(address => Profile) private _profile;
    mapping(bytes32 => address) private _handleOwner;

    error HandleTaken();
    error MismatchedLinks();
    error EmptyHandle();

    function setProfile(
        string calldata handle,
        string calldata blurb,
        string[] calldata linkLabels,
        string[] calldata linkUrls
    ) external {
        if (bytes(handle).length == 0) revert EmptyHandle();
        if (linkLabels.length != linkUrls.length) revert MismatchedLinks();

        bytes32 key = keccak256(bytes(handle));
        address owner = _handleOwner[key];
        if (owner != address(0) && owner != msg.sender) revert HandleTaken();

        Profile storage profile = _profile[msg.sender];
        if (profile.joinedAt == 0) profile.joinedAt = uint64(block.timestamp);
        if (bytes(profile.handle).length != 0) {
            _handleOwner[keccak256(bytes(profile.handle))] = address(0);
        }

        profile.handle = handle;
        profile.blurb = blurb;
        profile.linkLabels = linkLabels;
        profile.linkUrls = linkUrls;
        _handleOwner[key] = msg.sender;

        emit ProfileSet(msg.sender, handle);
    }

    function profileOf(address author) external view returns (Profile memory) {
        return _profile[author];
    }

    function authorOfHandle(string calldata handle) external view returns (address) {
        return _handleOwner[keccak256(bytes(handle))];
    }
}
