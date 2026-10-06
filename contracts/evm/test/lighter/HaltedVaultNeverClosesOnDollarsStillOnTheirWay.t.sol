// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, ValuationStatus, VaultState} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";

/// A halted vault closes at the first priced valuation that finds its
/// station empty, and its holders are then paid their part of the cash as it
/// stands when each of them asks.
///
/// Lighter takes a withdrawal out of the account as soon as it carries the
/// request out; its contract credits the station's pending balance minutes
/// later, when the batch is executed on-chain. An agent winding down inside
/// its grace may ask for the account's dollars in its last minutes. Those
/// dollars are counted from the moment they are asked for until they are
/// home (a post names the ones carried out and not yet credited), and a post
/// read before the exchange could have carried the request out prices
/// nothing, so the vault is never found empty while they are on their way.
contract HaltedVaultNeverClosesOnDollarsStillOnTheirWayTest is LighterVault {
    function setUp() public override {
        super.setUp();
        exchange.setCreditLater(true);
    }

    function _status() internal view returns (ValuationStatus) {
        return vault.valuationAt(vault.valuationCount() - 1).status;
    }

    /// Alice and Bob each put in 100k; 150k sits in the Lighter account as
    /// dollars, 50k stays in the vault. The vault is killed.
    function _killed() internal returns (uint256 aliceShares) {
        aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.approveKey(LIGHTER, executor);
        station.pull(150_000e6);
        station.deposit(LIGHTER, 150_000e6);
        station.approveKey(LIGHTER, executor);
        vm.stopPrank();
        skip(lighter.DELIVERY_WINDOW());
        _post();
        vm.prank(admin);
        vault.kill();
    }

    /// Three minutes before the grace ends the agent asks for everything
    /// back and the exchange carries the request out at once; the batch is
    /// not executed on-chain before the grace ends. The executor posts every
    /// minute; the keeper stops the station and values once the grace is
    /// over.
    function _haltedWithItsDollarsOnTheirWay() internal returns (uint256 aliceShares) {
        aliceShares = _killed();
        skip(HALT_GRACE - 3 minutes);
        vm.prank(executor);
        station.withdraw(LIGHTER, 150_000e6);
        exchange.process();
        skip(1 minutes);
        _post();
        skip(1 minutes);
        _post();
        skip(1 minutes);
        station.stop();
        value(vault);
    }

    function test_a_halted_vault_is_not_found_empty_while_a_withdrawal_is_on_its_way() public {
        _haltedWithItsDollarsOnTheirWay();
        assertEq(uint256(_status()), uint256(ValuationStatus.Priced), "the post read after the ask prices");
        assertGe(
            vault.valuationAt(vault.valuationCount() - 1).gross, 199_000e6, "the 150k on its way is counted"
        );
        vm.expectRevert(IVault.NotYet.selector);
        vault.close();
        assertTrue(vault.state() != VaultState.Closed, "the vault is still open");
    }

    function test_inside_its_grace_a_post_read_before_the_ask_could_be_carried_out_prices_nothing() public {
        _killed();
        skip(HALT_GRACE / 2);
        vm.prank(executor);
        station.withdraw(LIGHTER, 150_000e6);
        exchange.process();
        // The newest post was read before the ask: it can't tell whether the
        // exchange carried it out, so it prices nothing.
        value(vault);
        assertEq(uint256(_status()), uint256(ValuationStatus.Void), "a post from before the ask priced");
        vm.expectRevert(IVault.NotYet.selector);
        vault.close();
        // A post read sooner than the settle delay after the ask neither.
        skip(1 minutes);
        _post();
        value(vault);
        assertEq(
            uint256(_status()), uint256(ValuationStatus.Void), "a post read too soon after the ask priced"
        );
        // One read after it names the dollars on their way: priced, not empty.
        skip(lighter.SETTLE_DELAY());
        _post();
        value(vault);
        assertEq(uint256(_status()), uint256(ValuationStatus.Priced), "a post read after the delay prices");
        vm.expectRevert(IVault.NotYet.selector);
        vault.close();
    }

    function test_holders_of_a_halted_vault_are_paid_alike_whenever_they_ask() public {
        uint256 aliceShares = _haltedWithItsDollarsOnTheirWay();
        // The batch is executed on-chain, the pending balance paid out and
        // sent home with the station's next unwind.
        exchange.credit();
        _collect();
        station.unwind(1e18);
        assertEq(usd.balanceOf(address(station)), 0, "everything idle went home");
        skip(lighter.SETTLE_DELAY());
        _post();
        value(vault);
        assertEq(uint256(_status()), uint256(ValuationStatus.Priced), "priced once the dollars are home");
        vault.close();
        assertEq(uint256(vault.state()), uint256(VaultState.Closed), "closed once home");

        vm.prank(alice);
        uint256 toAlice = vault.redeemClosed(aliceShares, alice);
        uint256 bobShares = vault.balanceOf(bob);
        vm.prank(bob);
        uint256 toBob = vault.redeemClosed(bobShares, bob);
        assertApproxEqRel(toAlice, toBob, 0.001e18, "the same stake was paid differently by when it asked");
        assertGe(toAlice, 99_000e6, "each is paid their half");
    }
}
