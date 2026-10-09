// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// The leaver bears their own close's slippage through the measure, so what
/// stays in the account after a close is the stayers' part of it whatever
/// the fill: the release check asks only that this covers the venue's
/// transfer requirement on what stays open. A holder who is nearly the whole
/// vault can therefore leave while the agent holds a perp, and is paid what
/// their close fetched.
contract ANearlyWholeLeaverIsStagedWhileThePerpIsOpenTest is HyperliquidVault {
    /// Alice is nearly the whole vault; the executor moves almost all of it
    /// to HyperCore and goes long about 90k of ETH, about once the account.
    function _fundNearlyAlone() internal returns (uint256 aliceShares) {
        aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 10e6);
        vm.startPrank(executor);
        station.pull(90_000e6);
        station.deposit(HL, 90_000e6);
        vm.stopPrank();
        // 33.3333 ETH at $2,700.
        core.trade(address(station), ETH, 333_333, ETH_PX);
        _spread(ETH_PX, 10);
        skip(hl.DELIVERY_WINDOW());
        _stockGas();
    }

    function test_a_leaver_who_is_nearly_the_whole_vault_is_staged_and_paid_while_a_perp_is_open() public {
        uint256 aliceShares = _fundNearlyAlone();
        uint256 worth = _worthOf(alice);
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Closing, "her close is staged");

        uint256 before = usd.balanceOf(alice);
        _settleStaged();
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "paid from her own close");
        uint256 paid = usd.balanceOf(alice) - before;
        assertApproxEqRel(paid, worth, 0.01e18, "about what her shares were worth");
        assertLe(paid, worth, "never more than her shares were worth");
    }

    function test_a_leaver_whose_stayers_part_could_not_cover_the_transfer_requirement_is_still_refused()
        public
    {
        // Twelve times the account: what stays after any partial close can't
        // cover a tenth of what stays open, so nothing is sold.
        aliceShares_ = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(190_000e6);
        station.deposit(HL, 190_000e6);
        vm.stopPrank();
        core.trade(address(station), ETH, -8_444_444, ETH_PX);
        _spread(ETH_PX, 10);
        skip(hl.DELIVERY_WINDOW());
        _stockGas();
        _aliceClaims(aliceShares_);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "refused: nothing staged");
        assertEq(core.queued(), 0, "nothing ordered");
    }

    uint256 private aliceShares_;
}
