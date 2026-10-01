// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, Request} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// A leaver taking the fallback while a claim is queued is paid their share
/// of the free cash over the shares that own it: a queued claim's shares own
/// only the cash set aside for them.
contract FallbackSharesTheFreeCashTest is TwoChainVault {
    function test_a_fallback_taker_gets_their_share_of_the_free_cash_when_a_claim_is_queued() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        uint256 bobShares = depositTwo(bob, 100_000e6);
        depositTwo(author, 100_000e6);

        // Most of the money is at the home station as idle; 50k stays home.
        vm.prank(executor);
        station.pull(250_000e6);

        // Alice's withdrawal can't be paid from cash: a claim.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        valueWith(0, 0);
        assertEq(vault.claimCount(), 1);
        assertTrue(vault.claimAt(1).state == ClaimState.Pending);

        // Her fraction is closed at the home station and queued: what came
        // home plus her part of the cash is reserved for her.
        uint256 cashBefore = vault.cash();
        uint256 supply = vault.totalSupply();
        uint256 claimShares = vault.claimAt(1).shares;
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);
        uint256 cashAfter = vault.cash();
        uint256 home = cashAfter - cashBefore;
        // `cashShare` exactly as `settleClaim` computes it (no units, no
        // closed shares yet).
        uint256 cashShare = (cashBefore * claimShares) / supply;
        uint256 reserved = home + cashShare;
        if (reserved > cashAfter) reserved = cashAfter;
        uint256 free = cashAfter - reserved;
        assertEq(vault.totalSupply(), supply, "settleClaim changed the supply");

        // Bob asks to leave; the other chain goes quiet for a day, so
        // nothing can price him and the fallback opens.
        vm.prank(bob);
        uint256 request = vault.requestRedeem(bobShares, bob);
        Request memory r = vault.requestAt(request);
        vm.warp(r.earliest + 24 hours + 1);

        uint256 before = usd.balanceOf(bob);
        vm.prank(bob);
        vault.takeProRata(request);
        uint256 paid = usd.balanceOf(bob) - before;

        // The free cash is owned by the shares whose fraction is still at
        // the stations: everyone but the queued claim. Bob's part of it is
        // his shares over those, as `settleClaim` would have measured it.
        uint256 fair = (free * bobShares) / (supply - claimShares);
        uint256 actual = (free * bobShares) / supply;
        emit log_named_uint("free cash", free);
        emit log_named_uint("paid to bob", paid);
        emit log_named_uint("bob's fraction of the free cash", fair);
        emit log_named_uint("free * shares / totalSupply", actual);
        assertEq(
            paid,
            fair,
            "the fallback paid bob his share of the free cash over every share, including the queued claim's, which owns none of it"
        );
    }
}
