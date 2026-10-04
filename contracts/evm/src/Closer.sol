// SPDX-License-Identifier: BUSL-1.1
pragma solidity 0.8.30;

import {ICloser} from "./interfaces/ICloser.sol";
import {IStation, IStationVault} from "./interfaces/IStation.sol";
import {IVault} from "./interfaces/IVault.sol";

/// Holds nothing and answers to nobody: it only makes calls anyone could make
/// one at a time, and either all of them go through or none does.
contract Closer is ICloser {
    uint256 private constant WAD = 1e18;

    function closeHalted(address vault) external {
        IVault v = IVault(vault);
        IStation station = IStation(v.homeStation());
        if (!station.stopped()) station.stop();
        // A stopped station unwinds only once the last priced valuation has
        // settled, so what the sale brings home never pays a withdrawal
        // priced before it.
        if (IStationVault(vault).unsettled()) v.settle(type(uint256).max);
        (uint256 mark,) = station.value();
        if (mark != 0) station.unwind(WAD);
        // Anyone may value the vault, and only once a slot: a valuation
        // somebody else recorded earlier in this slot is the one the close
        // is judged on, rather than a reason to give up.
        if (!valuedThisSlot(v)) v.recordValuation();
        v.settle(type(uint256).max);
        v.close();
    }

    /// Whether the vault's latest valuation is for the current slot.
    function valuedThisSlot(IVault v) private view returns (bool) {
        uint256 count = v.valuationCount();
        if (count == 0) return false;
        uint256 interval = v.timers().slotInterval;
        uint256 slot = block.timestamp - (block.timestamp % interval);
        return v.valuationAt(count - 1).time >= slot;
    }
}
