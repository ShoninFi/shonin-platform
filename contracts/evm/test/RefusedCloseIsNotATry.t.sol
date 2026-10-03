// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";
import {FaultingDriver} from "./FaultingDriverEscape.t.sol";

/// A claim's close that the venue refuses outright sells nothing and brings
/// nothing home, like a close whose proceeds stay at the venue: it is not a
/// try, the claim waits as it was, and its freeze clock keeps running from
/// its booking.
contract RefusedCloseIsNotATryTest is ClaimBase {
    function test_a_close_the_venue_refuses_leaves_the_claim_as_it_was() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        uint256 cash = vault.cash();
        deploySpot(cash * 94 / 100, cash * 94 / 100, (cash * 94 / 100) * 9 / 10 * 1e18 / PRICE);
        uint256 id = claimFor(bob, bobShares);
        uint256 size = spotSize();
        uint256 cashBefore = vault.cash();

        pinnedDriverActsAs(address(new FaultingDriver()));
        vault.settleClaim(id);

        assertTrue(vault.claimAt(id).state == ClaimState.Waiting, "the claim moved on");
        assertEq(vault.claimAt(id).lastAttemptAt, 0, "a refused close counted as a try");
        assertEq(spotSize(), size, "something was sold");
        assertEq(vault.cash(), cashBefore, "the cash moved");
        assertEq(vault.balanceOf(bob), 0);
        assertEq(vault.claimAt(id).shares, bobShares, "the claim's shares changed");

        // A week from its booking, unpaid, anyone may freeze the vault.
        skip(7 days);
        vm.prank(alice);
        vault.freeze(id);
    }
}
