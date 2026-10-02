// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault, ValuationStatus, VaultState} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// Once a halted vault's valuation has found the stations empty, a pending
/// claim can't be tried any more: it is paid from the closed vault, and a
/// try could only hold the close off.
contract ClaimsWaitForTheCloseTest is TwoChainVault {
    function test_a_pending_claim_waits_for_the_close() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        depositTwo(bob, 100_000e6);
        vm.prank(executor);
        station.pull(190_000e6);

        // Alice's withdrawal becomes a claim; nobody serves it.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        valueWith(0, 0);
        assertTrue(vault.claimAt(1).state == ClaimState.Pending);

        // The admin kills the vault; the home station is stopped and every
        // dollar comes home; the other chain posts once more, empty.
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);
        valueWith(0, 0);
        assertEq(latest().gross, latest().cash + 1, "the stations were not found empty");

        // A passer-by tries the claim: refused, and the vault closes.
        vm.prank(bob);
        vm.expectRevert(IVault.WrongState.selector);
        vault.settleClaim(1);
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);

        // Alice is paid her fraction of the closed vault.
        vm.prank(alice);
        vault.claimClosed(1);
        assertGt(usd.balanceOf(alice), 0);
    }
}
