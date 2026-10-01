// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A claim nobody could serve for a day takes the pro-rata fallback, counted
/// from its booking, even while valuations keep being priced.
contract ClaimFallbackFromBookingTest is ClaimBase {
    function test_a_claim_nothing_priced_for_a_day_can_take_the_fallback() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        // 180k of BTC at the venue, 10k of venue cash, 10k of vault cash.
        deploySpot(190_000e6, 190_000e6, 3e18);
        uint256 id = claimFor(alice, aliceShares);

        // The market sits 20% wide around an unchanged mark for days: every
        // try finds it outside the band.
        setPrice(SPOT, PRICE, 2000);
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Waiting);

        // Three days of valuations, one every ten minutes, each priced and
        // each `wide`. None of them prices her claim.
        for (uint256 i = 0; i < 3 * 24 * 6; i++) {
            skip(10 minutes);
            value(vault);
        }
        assertTrue(vault.claimAt(id).state == ClaimState.Waiting, "the claim was priced after all");

        // Three days without being priced: section 8 opens the fallback.
        vm.prank(alice);
        vault.takeProRataClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Fallback);
        assertGt(usd.balanceOf(alice), 0, "she took nothing");
    }
}
