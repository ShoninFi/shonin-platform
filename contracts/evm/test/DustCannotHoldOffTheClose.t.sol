// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, VaultState} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// Dust at a station (up to a tenth of the minimum deposit) doesn't count
/// as holdings, so a stranger's micro-dollar can't hold a halted vault's
/// close off.
contract DustCannotHoldOffTheCloseTest is TwoChainVault {
    function test_a_strangers_micro_dollar_at_the_home_station_cannot_hold_off_the_close() public {
        depositTwo(alice, 100_000e6);
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);

        // The keeper records the closing valuation; alice, a stranger, sends
        // one unit of the dollar to the home station in the block before.
        give(address(station), 1);
        valueWith(0, 0);
        assertFalse(vault.state() == VaultState.Closed);
        (bool ok,) = address(vault).call(abi.encodeCall(IVault.close, ()));
        assertTrue(ok, "a stranger's micro-dollar at the station held off the close");
    }
}
