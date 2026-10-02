// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A closing vault pays its holders through its own door: a redeem asked
/// for just before the valuation that finds the stations empty can't take
/// the fee-free fallback; the next valuation prices it, fee and all, and the
/// vault closes.
contract ClosingVaultRefusesTheFallbackTest is ClaimBase {
    function test_a_redeem_asked_just_before_the_empty_valuation_pays_the_fee() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 3e18);
        // A fresh fee week, then a gain: the pool is above its mark.
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        skip(1 days);
        setPrice(SPOT, PRICE * 120 / 100, 0);
        value(vault);
        assertGt(latest().fee, 0, "no fee is owed");

        // The admin kills; the station is stopped and fully unwound.
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);

        // Alice asks to redeem two seconds before the slot: her request is
        // priced at the slot after, not this one.
        uint256 t = vm.getBlockTimestamp();
        uint256 slot = t - (t % SLOT) + SLOT;
        vm.warp(slot - 2);
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        vm.warp(slot);
        vault.recordValuation();
        vault.settle(type(uint256).max);
        assertFalse(vault.requestAt(id).done, "her request was priced after all");
        // The vault is now ready to close: no new request is taken...
        vm.prank(bob);
        vm.expectRevert(IVault.WrongState.selector);
        vault.requestRedeem(1, bob);
        // ...and a claim's fallback would be refused. The close waits for
        // her request.
        vm.expectRevert(IVault.Unsettled.selector);
        vault.close();

        // A day passes with no valuation recorded. Her redeem's fallback is
        // open: her fraction of the cash, no fee.
        skip(1 days + SLOT);
        uint256 cash = vault.cash();
        uint256 supply = vault.totalSupply();
        vm.prank(alice);
        vm.expectRevert(IVault.WrongState.selector);
        vault.takeProRata(id);
        // The next valuation prices her redeem, fee and all.
        value(vault);
        uint256 paid = usd.balanceOf(alice);
        emit log_named_uint("alice took through the fallback", paid);
        emit log_named_uint("her fraction of the cash", cash * aliceShares / supply);

        // Everyone else is paid after the close's payday.
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);
        uint256 bobShares = vault.balanceOf(bob);
        vm.prank(bob);
        vault.redeemClosed(bobShares, bob);
        uint256 bobPaid = usd.balanceOf(bob);
        emit log_named_uint("bob was paid through the closed door", bobPaid);
        emit log_named_uint(
            "fee shares minted at the close", vault.balanceOf(author) + vault.balanceOf(platform)
        );

        // Same shares, same cash: alice left with more than bob because she
        // skipped the profit fee the close charged him.
        assertLe(paid, bobPaid + 1, "a redeem's fallback while the vault was closing skipped the close's fee");
    }
}
