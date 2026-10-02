// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, Valuation, ValuationStatus} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Nobody is paid at a price the vault would refuse to sell at. When a
/// station's exit side sits further below its mark than the exit band at a
/// valuation, a withdrawal the cash could pay waits as a claim instead, and
/// is paid its fraction once the market is back inside the band; when the
/// cash is short, the close itself is refused. Whoever widens a spread for
/// one block takes nothing from a leaver.
contract NobodyPaidOutsideTheBandTest is ClaimBase {
    uint256 internal constant BPS = 10_000;

    /// Alice and bob hold half each. Half the vault is at the venue in spot
    /// BTC; half is cash, so alice's withdrawal is paid from cash.
    function halfAtTheVenue() internal returns (uint256 aliceShares, uint256 bobShares) {
        aliceShares = depositAs(vault, alice, 100_000e6);
        bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(100_000e6, 100_000e6, 1.5e18);
        value(vault);
        assertApproxEqAbs(vault.totalValue(), 200_000e6, 1e6);
    }

    function test_a_leaver_the_cash_could_pay_waits_while_the_market_is_outside_the_band() public {
        (uint256 aliceShares, uint256 bobShares) = halfAtTheVenue();
        uint256 bobBefore = worth(bobShares);

        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);

        // At the slot, the spread is 20% around an unchanged mark and
        // oracle: the bid is 54k on a 60k BTC. A close inside the 1% band
        // would be refused at this price.
        setPrice(SPOT, PRICE, 2000);
        value(vault);
        // That valuation prices nothing: she is not paid at that price.
        assertTrue(latest().status == ValuationStatus.Void, "a valuation outside the band was priced");
        assertEq(usd.balanceOf(alice), 0);

        // The spread closes again, and the next valuation pays her fraction.
        setPrice(SPOT, PRICE, 0);
        value(vault);
        uint256 paid = usd.balanceOf(alice);
        assertApproxEqRel(paid, 100_000e6 * 999 / 1000, 0.001e18);
        assertGe(worth(bobShares) + 1e6, bobBefore, "the holder who stayed paid for her exit");
    }

    /// The same spread when the cash is short: the claim path refuses to
    /// sell, so the leaver keeps their shares and waits.
    function test_the_same_spread_blocks_a_claim_instead() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 3e18);
        uint256 id = claimFor(alice, aliceShares);
        setPrice(SPOT, PRICE, 2000);
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Waiting, "the claim was sold through the spread");
        assertEq(usd.balanceOf(alice), 0);
    }
}
