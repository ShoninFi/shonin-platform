// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalUsd} from "../src/LocalUsd.sol";
import {ClaimState, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// The stand-in dollar with an issuer's blocklist and pause.
contract BlockingUsd is LocalUsd {
    mapping(address => bool) public blocked;
    bool public paused;

    function setBlocked(address who, bool on) external {
        blocked[who] = on;
    }

    function setPaused(bool on) external {
        paused = on;
    }

    function _update(address from, address to, uint256 value) internal override {
        require(!paused, "paused");
        require(!blocked[from] && !blocked[to], "blocked");
        super._update(from, to, value);
    }
}

/// A dollar's issuer may blocklist a holder or pause every transfer. A
/// blocklisted leaver's payout is held for them while everyone else is
/// paid; a pause holds payouts and moves no money, and once it is lifted
/// the halted vault unwinds and closes holding all it owes.
contract ABlocklistOrPauseLosesNobodyMoneyTest is ClaimBase {
    function _blockingUsd() private returns (BlockingUsd b) {
        vm.etch(address(usd), address(new BlockingUsd()).code);
        b = BlockingUsd(address(usd));
    }

    function test_a_blocklisted_leavers_payout_is_held_and_the_rest_are_paid() public {
        depositAs(vault, alice, 1000e6);
        depositAs(vault, bob, 1000e6);
        BlockingUsd b = _blockingUsd();
        uint256 __s_alice = vault.balanceOf(alice);
        vm.prank(alice);
        vault.requestRedeem(__s_alice, alice);
        uint256 __s_bob = vault.balanceOf(bob);
        vm.prank(bob);
        vault.requestRedeem(__s_bob, bob);
        b.setBlocked(bob, true);
        uint256 aliceBefore = usd.balanceOf(alice);
        value(vault);
        assertGt(usd.balanceOf(alice), aliceBefore, "alice paid");
        assertGt(vault.heldFor(bob), 0, "bob held");
        assertEq(vault.nextUnpriced(), 0, "queue not stuck");
        vm.prank(bob);
        vm.expectRevert();
        vault.collectHeld();
        b.setBlocked(bob, false);
        uint256 held = vault.heldFor(bob);
        vm.prank(bob);
        assertEq(vault.collectHeld(), held);
        assertEq(vault.heldFor(bob), 0);
    }

    function test_a_paused_dollar_loses_nobody_money_through_a_kill_and_close() public {
        depositAs(vault, alice, 1000e6);
        deploySpot(500e6, 400e6, 0.005e8);
        BlockingUsd b = _blockingUsd();
        uint256 __s_alice = vault.balanceOf(alice);
        vm.prank(alice);
        vault.requestRedeem(__s_alice, alice);
        b.setPaused(true);
        // The valuation and the settlement go through; the payout is held.
        value(vault);
        // The redeem is a claim (cash can't cover it) or held; either way the
        // accounting is intact.
        uint256 claims = vault.claimCount();
        if (claims != 0) {
            // A claim's close moves dollars from the venue: paused, it fails,
            // and the claim waits as it was (not a try, no money moved).
            vault.settleClaim(1);
            ClaimState st = vault.claimAt(1).state;
            assertTrue(st == ClaimState.Pending || st == ClaimState.Waiting, "claim untouched");
        }
        // The admin kills while paused: no transfer happens in `kill`.
        vm.prank(admin);
        vault.kill();
        assertEq(uint256(vault.state()), uint256(VaultState.Killed));
        // Unpaused, the full unwind and the close pay everyone from what is
        // there; conservation: the vault holds at least what it owes.
        b.setPaused(false);
        skip(HALT_GRACE + 1);
        station.stop();
        station.unwindStopped(new bytes32[](0));
        bytes32[] memory drivers = new bytes32[](1);
        drivers[0] = "local";
        station.unwindStopped(drivers);
        value(vault);
        vault.close();
        uint256 owed = vault.cash() + vault.escrow() + vault.tank().balance + vault.heldFor(alice)
            + vault.heldFor(author) + vault.heldFor(platform);
        (, uint256 reserve) = vault.units();
        assertGe(usd.balanceOf(address(vault)), owed + reserve, "vault holds what it owes");
    }
}
