// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IStation, StationSpec} from "../interfaces/IStation.sol";
import {IVault, Timers} from "../interfaces/IVault.sol";

/// What the drivers whose venue acts after the transaction share: the name a
/// station pinned them under, and how far apart moves of dollars through
/// them must be.
library DelayedVenue {
    /// A move of dollars sooner than the spacing allows.
    error TooSoon();

    /// The name `station` pinned `driver` under (zero if none): what its
    /// counters for the venue are kept under. Read from the station, not
    /// assumed, so the same code approved under any name counts its flows.
    function nameIn(address station, address driver) internal view returns (bytes32) {
        StationSpec memory spec = IStation(station).spec();
        for (uint256 i = 0; i < spec.drivers.length; i++) {
            if (IStation(station).pinnedDriver(spec.drivers[i]) == driver) return spec.drivers[i];
        }
        return bytes32(0);
    }

    /// When the next move of dollars through a driver may be made, if the
    /// last one voids valuations until `pricedFrom` (zero: nothing does):
    /// one slot and its window of the station's vault later, so between two
    /// moves there is always a whole slot window in which a valuation can be
    /// priced. A station on another chain than its vault's can't read the
    /// vault's slots: there it is `REMOTE_SLOT` later.
    function slotAfter(address station, uint256 pricedFrom) internal view returns (uint256) {
        if (pricedFrom == 0) return 0;
        StationSpec memory spec = IStation(station).spec();
        uint256 slot = REMOTE_SLOT;
        if (spec.vaultChain == block.chainid) {
            Timers memory timers = IVault(spec.vault).timers();
            slot = uint256(timers.slotInterval) + timers.slotWindow;
        }
        return pricedFrom + slot;
    }

    /// Refuse a move sooner than `movableAt`.
    function spaced(uint256 movableAt) internal view {
        if (block.timestamp < movableAt) revert TooSoon();
    }

    uint256 internal constant REMOTE_SLOT = 1 hours;
}
