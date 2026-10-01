// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault, Tank} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// Edges of the claim path: a refused payout, and the holding fee on a
/// claim served long after the last valuation.
contract ClaimEdgesTest is ClaimBase {
    function assertSolvent(string memory when) internal view {
        Tank memory t = vault.tank();
        (, uint256 reserve) = vault.units();
        uint256 booked = vault.cash() + t.balance + vault.escrow() + reserve + vault.heldFor(alice)
            + vault.heldFor(bob) + vault.heldFor(carol) + vault.heldFor(author) + vault.heldFor(platform);
        assertGe(usd.balanceOf(address(vault)), booked, when);
    }

    /// The dollar refuses the leaver: the claim is still paid (held), the
    /// fee still reaches the author and the platform, nothing reverts, and
    /// the vault stays solvent until the leaver collects.
    function test_a_claim_whose_leaver_the_dollar_refuses_is_held_and_the_vault_stays_solvent() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 2e18);
        give(address(station), 20_000e6); // above the mark: a fee is owed
        uint256 id = claimFor(alice, aliceShares);
        uint256 fair = worth(aliceShares);
        uint256 bobBefore = perShare();

        vm.mockCallRevert(address(usd), abi.encodeWithSelector(IERC20.transfer.selector, alice), "blocked");
        vault.settleClaim(id);
        vm.clearMockedCalls();

        assertTrue(vault.claimAt(id).state == ClaimState.Paid);
        assertEq(usd.balanceOf(alice), 0);
        uint256 held = vault.heldFor(alice);
        assertGt(held, 0, "nothing was held for her");
        assertLe(held, fair);
        assertGt(usd.balanceOf(author), 0, "the author's fee was not paid");
        assertGt(usd.balanceOf(platform), 0, "the platform's fee was not paid");
        assertGe(perShare() + 1, bobBefore);
        assertSolvent("with the payout held");
        vm.prank(alice);
        vault.collectHeld();
        assertEq(usd.balanceOf(alice), held);
        assertSolvent("after she collected");
    }

    /// Thirty days pass with no valuation after the claim is booked. The
    /// close path syncs the holding fee first, so the leaver bears thirty
    /// days of it (1% a year: 0.082%) like everyone else.
    function test_a_claim_served_a_month_later_bears_a_month_of_holding_fee() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 2e18);
        uint256 id = claimFor(alice, aliceShares);
        uint256 fairNow = worth(aliceShares);
        skip(30 days);
        uint256 supplyBefore = vault.totalSupply();
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid);
        uint256 paid = usd.balanceOf(alice);
        uint256 minted = vault.totalSupply() + aliceShares - supplyBefore;
        emit log_named_uint("holding shares minted at the try", minted);
        // 0.082% of holding fee and 0.1% of transaction fee off her worth.
        uint256 expected = fairNow * 999 / 1000;
        expected = expected - expected * 30 / 36_500;
        assertApproxEqRel(paid, expected, 0.0002e18, "the leaver did not bear the holding fee");
        assertLt(paid, fairNow * 999 / 1000, "the leaver skipped the holding fee");
    }
}
