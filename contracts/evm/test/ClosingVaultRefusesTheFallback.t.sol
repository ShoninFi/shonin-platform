// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A closing vault pays its holders through its own door: a redeem asked
/// for just before the valuation that finds the stations empty can't take
/// the fallback and doesn't hold off the close; it is settled after the
/// close at the closed vault's price, fee and all, as everyone else is.
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
        // ...her fallback is refused...
        skip(1 days + SLOT);
        vm.prank(alice);
        vm.expectRevert(IVault.WrongState.selector);
        vault.takeProRata(id);
        // ...and her request doesn't hold off the close.
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);
        // Anyone settles it at the closed vault's price.
        vault.settleClosed(id);
        assertTrue(vault.requestAt(id).done);
        uint256 paid = usd.balanceOf(alice);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.settleClosed(id);

        // Bob, with the same shares, is paid through the closed door.
        uint256 bobShares = vault.balanceOf(bob);
        vm.prank(bob);
        vault.redeemClosed(bobShares, bob);
        uint256 bobPaid = usd.balanceOf(bob);
        emit log_named_uint("bob was paid through the closed door", bobPaid);
        emit log_named_uint(
            "fee shares minted at the close", vault.balanceOf(author) + vault.balanceOf(platform)
        );

        // One price for both, after the close's fee.
        assertApproxEqRel(paid * bobShares, bobPaid * aliceShares, 1e9, "the two were paid different prices");
        assertGt(vault.balanceOf(author) + vault.balanceOf(platform), 0, "the close charged no fee");
    }
}
