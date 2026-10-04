// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// Closes a halted vault in one transaction: settles what is owed, stops and
/// empties its station, values it and closes it. Every step is one anyone
/// may take; taking them together means nobody can get between them, such as
/// by sending the stopped station a little money before each valuation so
/// that it never reads as empty. A valuation somebody else recorded earlier
/// in the slot is used rather than refused, since there is one a slot.
interface ICloser {
    function closeHalted(address vault) external;
}
