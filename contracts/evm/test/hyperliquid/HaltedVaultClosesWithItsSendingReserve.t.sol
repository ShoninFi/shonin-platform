// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {VaultState} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "../hyperliquid/HyperliquidVault.sol";

/// A Hyperliquid account keeps HYPE to pay for sends home. The driver never
/// sells it, in any close, so it is worth nothing in what the account is
/// worth. A halted vault closes only once a valuation finds its station
/// empty but for dust (a tenth of the minimum deposit, at most ten dollars):
/// the HYPE a stopped station is left with, which nothing can ever bring
/// home, never keeps the vault from closing.
contract HaltedVaultClosesWithItsSendingReserveTest is HyperliquidVault {
    function test_a_halted_vault_whose_station_holds_only_its_hype_for_sends_home_closes() public {
        _fundAndTrade();
        // What one top-up by the executor leaves there: $12 of HYPE, the
        // venue's smallest order, well under the reserve of one HYPE.
        core.creditSpot(address(station), HYPE_TOKEN, 0.6e8);

        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();

        // The keeper's repeated unwind: place the closes, send the freed
        // dollars to HyperEVM, send them home.
        station.unwind(1e18);
        _nextBlock();
        station.unwind(1e18);
        _nextBlock();
        station.unwind(1e18);
        _nextBlock();
        (uint256 home, bool blocked) = station.unwind(1e18);
        assertEq(home, 0, "nothing more comes home");
        assertFalse(blocked, "and nothing is left to close");
        assertEq(core.szi(address(station), ETH), 0, "no position");
        assertEq(usd.balanceOf(address(station)), 0, "no idle dollars");
        assertLt(core.rawUsd(address(station)), 1e6, "no dollars at the venue");
        assertGt(vault.cash(), 185_000e6, "the account's worth is home");

        // All that is left is the HYPE no close ever sells, worth nothing.
        assertEq(core.spotTotal(address(station), HYPE_TOKEN) > 0.5e8, true, "the HYPE is still there");
        (uint256 mark,) = station.value();
        assertEq(mark, 0, "the station holds nothing anyone can sell");

        // Once the dollars it last sent home have had time to land, a
        // valuation finds it empty and the vault closes.
        skip(hl.CREDIT_WINDOW());
        value(vault);
        vault.close();
        assertTrue(vault.state() == VaultState.Closed, "the halted vault closes");
    }
}
