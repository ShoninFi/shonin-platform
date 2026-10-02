// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, Request} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// On a vault that trades on other chains too, units worth nothing on any
/// station are cleared by a serve that brings nothing home, so dust taken
/// through the fallback never blocks a pull.
contract DustUnitsOnTwoChainsTest is TwoChainVault {
    function test_dust_units_on_a_two_chain_vault_are_cleared_and_never_block_a_pull() public {
        depositTwo(alice, 100_000e6);
        depositTwo(bob, 100_000e6);
        vm.prank(executor);
        station.pull(150_000e6);
        valueWith(0, 0);

        // Bob asks to redeem a sliver: its fraction of the stations is about
        // 1e3 wei of 1e18, worth nothing. The other chain goes quiet for a
        // day, so nothing prices it, and he takes the fallback.
        uint256 sliver = vault.totalSupply() / 1e15;
        vm.prank(bob);
        uint256 id = vault.requestRedeem(sliver, bob);
        Request memory r = vault.requestAt(id);
        vm.warp(r.earliest + 24 hours + 1);
        vm.prank(bob);
        vault.takeProRata(id);
        (uint256 units,) = vault.units();
        assertGt(units, 0, "no units were minted");
        assertLt(units, 1e6, "the units are not dust");

        // Worth nothing on any station, they are cleared by a serve that
        // brings nothing home.
        vault.serveProRata(bob);
        (units,) = vault.units();
        assertEq(units, 0, "the dust units were not cleared");

        // The other chain is back; valuations are priced again.
        valueWith(0, 0);
        valueWith(0, 0);

        // And the strategy can never pull a unit of cash again.
        vm.prank(executor);
        (bool ok,) = address(station).call(abi.encodeWithSignature("pull(uint256)", 1e6));
        assertTrue(ok, "dust units left by the fallback block the home station's pulls for ever");
    }
}
