// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, Request} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Moving the dead shares' part to the author's tank at the close leaves the
/// closed vault's books whole. With a pending claim, a redeem filed after the
/// last valuation, a payout the dollar refused, the fee shares the close's
/// payday mints and the dead shares, every door pays its holder their
/// fraction of the cash, the tank gains exactly the dead shares' part, and
/// nothing is left over.
contract CloseConservesEveryDollarTest is ClaimBase {
    function test_every_door_of_a_closed_vault_adds_up_to_the_cash_at_the_close() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        uint256 carolShares = depositCarol(50_000e6);
        deploySpot(230_000e6, 200_000e6, 2e18);

        // Bob's redeem can't be paid from cash: a claim, left pending.
        uint256 claimId = claimFor(bob, bobShares);

        // Carol's payout is refused by the dollar when she is paid: held.
        vm.mockCallRevert(address(usd), abi.encodeWithSelector(usd.transfer.selector, carol), "refused");

        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);
        assertEq(spotSize(), 0);

        // Alice files a redeem just before the valuation that finds the
        // stations empty, so it is settled after the close.
        uint256 t = vm.getBlockTimestamp();
        uint256 slot = t - (t % SLOT) + SLOT;
        vm.warp(slot - 4);
        vm.prank(alice);
        uint256 straggler = vault.requestRedeem(aliceShares / 2, alice);
        Request memory r = vault.requestAt(straggler);
        vm.warp(slot);
        vault.recordValuation();
        vault.settle(type(uint256).max);
        assertGt(r.earliest, vault.valuationAt(vault.valuationCount() - 1).time, "not a straggler");
        assertFalse(vault.requestAt(straggler).done);

        uint256 tankBefore = vault.tank().balance;
        uint256 cashBefore = vault.cash();
        vault.close();
        uint256 deadPart = vault.tank().balance - tankBefore;
        // The close's payday mints fee shares first, so the dead shares' part
        // is at most what they owned of the cash before it.
        assertLe(deadPart, cashBefore * 0.01e6 * 1e12 / (vault.totalSupply() + 0.01e6 * 1e12) + 1);
        assertEq(vault.balanceOf(address(0xdEaD)), 0);
        uint256 cashAtClose = vault.cash();
        assertEq(cashAtClose, cashBefore - deadPart, "the close moved other than the dead shares' part");

        // Every door.
        uint256 paid;
        paid += vault.claimClosed(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Paid);
        paid += vault.settleClosed(straggler);
        uint256 aliceLeft = vault.balanceOf(alice);
        vm.prank(alice);
        paid += vault.redeemClosed(aliceLeft, alice);
        vm.prank(carol);
        uint256 carolPaid = vault.redeemClosed(carolShares, carol);
        paid += carolPaid;
        vm.clearMockedCalls();
        assertEq(vault.heldFor(carol), carolPaid, "carol's refused payout wasn't held for her");
        vm.prank(carol);
        vault.collectHeld();
        assertEq(usd.balanceOf(carol), carolPaid);
        for (uint256 i = 0; i < 2; i++) {
            address holder = i == 0 ? platform : author;
            uint256 shares = vault.balanceOf(holder);
            if (shares == 0) continue;
            vm.prank(holder);
            paid += vault.redeemClosed(shares, holder);
        }
        assertEq(vault.totalSupply(), 0, "shares outlived every door");
        assertLe(vault.cash(), 4, "money was stranded");
        assertEq(paid + vault.cash(), cashAtClose, "the doors paid other than the cash at the close");
        // What the vault holds is the tank, nothing more: nothing was paid
        // twice.
        assertEq(usd.balanceOf(address(vault)), vault.tank().balance + vault.cash());
        // The author's tank can be taken at once.
        vm.prank(author);
        vault.withdrawTankUnsigned(type(uint256).max);
        assertEq(usd.balanceOf(address(vault)), vault.cash());
    }
}
