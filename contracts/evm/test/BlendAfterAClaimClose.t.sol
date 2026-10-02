// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, Valuation} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// The mark blend stays exact when a claim's close burns shares between a
/// valuation's pricing and its settlement: the fee owed moves only by the
/// profit fee on the transaction fee the leaver left in the pool.
contract BlendAfterAClaimCloseTest is ClaimBase {
    function netPerShare() internal view returns (uint256) {
        (uint256 markValue,) = station.value();
        uint256 total = vault.cash() + markValue + 1;
        uint256 supply = vault.totalSupply() + 1e12;
        uint256 basis = ceilDiv(vault.mark() * supply, 1e30);
        uint256 fee = total > basis ? (total - basis) * 1500 / 10_000 : 0;
        return (total - fee) * 1e18 / supply;
    }

    function feeOwed() internal view returns (uint256) {
        (uint256 markValue,) = station.value();
        uint256 total = vault.cash() + markValue + 1;
        uint256 supply = vault.totalSupply() + 1e12;
        uint256 basis = ceilDiv(vault.mark() * supply, 1e30);
        return total > basis ? (total - basis) * 1500 / 10_000 : 0;
    }

    function test_a_deposit_blends_the_mark_exactly_after_a_claim_closed_between_pricing_and_settlement()
        public
    {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 3e18);
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        skip(1 days);
        setPrice(SPOT, PRICE * 120 / 100, 0);
        value(vault);
        assertGt(latest().fee, 0, "no fee is owed");

        // Bob's whole holding can't be paid from 10k: a claim.
        uint256 id = claimFor(bob, bobShares);

        // Carol's deposit is priced at the next valuation; bob's claim is
        // closed after that valuation is priced and before carol's deposit
        // is settled.
        bytes memory statement = statementBy(carolKey, address(vault), carol, vault.restrictedText());
        give(carol, 100_000e6);
        vm.startPrank(carol);
        usd.approve(address(vault), 100_000e6);
        vault.requestDeposit(100_000e6, carol, statement);
        vm.stopPrank();
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        Valuation memory v = latest();
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid, "bob's claim did not close");
        uint256 feeBefore = feeOwed();
        uint256 aliceBefore = netPerShare();
        uint256 markBefore = vault.mark();

        vault.settle(type(uint256).max);
        uint256 carolShares = vault.balanceOf(carol);
        assertGt(carolShares, 0, "carol got no shares");

        emit log_named_uint("fee owed before carol joined", feeBefore);
        emit log_named_uint("fee owed after carol joined", feeOwed());
        emit log_named_uint("mark before", markBefore);
        emit log_named_uint("mark after the blend", vault.mark());
        emit log_named_uint("alice's net price before", aliceBefore);
        emit log_named_uint("alice's net price after", netPerShare());

        // The fee owed moves only by the fee on the kept transaction fee.
        uint256 kept = ceilDiv(100_000e6 * 10, 10_000);
        assertApproxEqAbs(feeOwed(), feeBefore + kept * 1500 / 10_000, 2, "the blend was not exact");
        assertGe(vault.mark(), markBefore, "the mark fell");
        assertGe(netPerShare() + 1, aliceBefore, "the passive holder paid for the deposit");
        // Carol bought at the snapshot's net price.
        uint256 net = v.gross - v.fee;
        uint256 expected = (100_000e6 - kept) * v.supply / net;
        assertApproxEqAbs(carolShares, expected, 1e6, "carol was not priced at the snapshot");
        aliceShares;
    }
}
