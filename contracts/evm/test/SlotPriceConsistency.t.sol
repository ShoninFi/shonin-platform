// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, Valuation} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";
import {Vm} from "forge-std/Vm.sol";

/// One slot, one price, with every door open at once (two deposits, a
/// redeem, a queued claim, the author's fill) at a valuation carrying a
/// premium and a fee owed, with and without payday.
contract SlotPriceConsistencyTest is ClaimBase {
    uint256 internal aliceShares;
    uint256 internal bobShares;
    uint256 internal carolShares;
    uint256 internal claimId;

    /// Three holders of a third each, 3 BTC at the venue, a fresh fee week,
    /// then BTC up 20% so a fee is owed. Bob's redeem is booked as a claim
    /// at a wide valuation, then the spread closes again.
    function aboveTheMarkWithAClaim() internal {
        aliceShares = depositAs(vault, alice, 100_000e6);
        bobShares = depositAs(vault, bob, 100_000e6);
        carolShares = depositCarol(100_000e6);
        // 150k of BTC and 20k at the venue; 130k of cash covers bob's claim
        // at the next valuation's price once the spread is back.
        deploySpot(170_000e6, 170_000e6, 2.5e18);
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        skip(1 days);
        setPrice(SPOT, PRICE * 120 / 100, 0);
        value(vault);
        assertGt(latest().fee, 0, "no fee is owed");

        // The strategy holds most of the cash at the station for a moment:
        // bob's withdrawal can't be paid and is booked as a claim. Then it
        // brings the cash home, so the claim is covered and queues.
        vm.prank(executor);
        station.pull(110_000e6);
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        value(vault);
        claimId = vault.claimCount();
        assertTrue(
            vault.claimAt(claimId).state == ClaimState.Pending, "bob's redeem was not booked as a claim"
        );
        vm.prank(executor);
        station.push(110_000e6);
    }

    /// The venue's bid a touch above its mark: the valuation carries `extra`.
    function bidAboveTheMark() internal {
        uint256 p = PRICE * 120 / 100;
        vm.prank(operator);
        venue.setPrices(SPOT, p, p, p * 1001 / 1000, p * 1002 / 1000);
    }

    function requestDeposit(address who, uint256 amount) internal {
        bytes memory statement =
            statementBy(who == carol ? carolKey : keyOf(who), address(vault), who, vault.restrictedText());
        give(who, amount);
        vm.startPrank(who);
        usd.approve(address(vault), amount);
        vault.requestDeposit(amount, who, statement);
        vm.stopPrank();
    }

    /// A passive holder's worth per share at the mark side, net of the
    /// profit fee owed, scaled by 1e18.
    function netPerShare() internal view returns (uint256) {
        (uint256 markValue,) = station.value();
        uint256 total = vault.cash() + markValue + 1;
        uint256 supply = vault.totalSupply() + 1e12;
        uint256 basis = ceilDiv(vault.mark() * supply, 1e30);
        uint256 fee = total > basis ? (total - basis) * 1500 / 10_000 : 0;
        return (total - fee) * 1e18 / supply;
    }

    function oneSlot(bool payday) internal {
        aboveTheMarkWithAClaim();

        // Everything lands in the queue for one slot: bob's claim, queued on
        // the cash (the 110k covers him at the last valuation's exit price),
        // carol's and alice's deposits, carol's redeem of a tenth of her
        // shares, and the author's fill from their fee shares. The exits are
        // paid from the cash the valuation saw, before the deposits join.
        vault.settleClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Queued, "bob's claim was not queued");
        requestDeposit(carol, 50_000e6);
        requestDeposit(alice, 20_000e6);
        uint256 carolRedeems = carolShares / 10;
        vm.prank(carol);
        vault.requestRedeem(carolRedeems, carol);
        uint256 authorHad = vault.balanceOf(author);
        assertGt(authorHad, 0, "the author holds no fee shares to fill from");
        vm.prank(author);
        vault.topUpFromShares(authorHad / 2);

        if (payday) vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        bidAboveTheMark();
        uint256 aliceBefore = vault.balanceOf(alice);
        uint256 stayerBefore = netPerShare();
        vm.recordLogs();
        value(vault);
        Valuation memory v = latest();
        uint256 stayerAfter = netPerShare();
        // The fill, from its event: the author's balance also receives the
        // other leavers' fee slices in this slot, so it can't be read from
        // the balance.
        (uint256 fillShares, uint256 filled) = tankFilled(vm.getRecordedLogs());

        // Deposits: shares per net dollar. Carol kept nine tenths of her old shares.
        uint256 carolGot = vault.balanceOf(carol) - (carolShares - carolRedeems);
        uint256 aliceGot = vault.balanceOf(alice) - aliceBefore;
        uint256 carolPerDollar = carolGot * 1e18 / (50_000e6 - 50e6);
        uint256 alicePerDollar = aliceGot * 1e18 / (20_000e6 - 20e6);
        emit log_named_uint("carol's shares per net dollar", carolPerDollar);
        emit log_named_uint("alice's shares per net dollar", alicePerDollar);
        assertApproxEqRel(carolPerDollar, alicePerDollar, 1e9, "two deposits in one slot paid two prices");

        // Withdrawals: dollars per share, for the claim, the redeem and the
        // fill.
        assertTrue(vault.claimAt(claimId).state == ClaimState.Paid, "bob's queued claim was not paid");
        uint256 bobPerShare = vault.claimAt(claimId).paid * 1e18 / bobShares;
        uint256 carolPerShare = usd.balanceOf(carol) * 1e18 / carolRedeems;
        assertGt(filled, 0, "the fill paid nothing");
        uint256 fillPerShare = filled * 1e18 / fillShares;
        emit log_named_uint("bob's claim, dollars per share", bobPerShare);
        emit log_named_uint("carol's redeem, dollars per share", carolPerShare);
        emit log_named_uint("the author's fill, dollars per share", fillPerShare);
        assertApproxEqRel(bobPerShare, carolPerShare, 1e9, "a claim and a redeem in one slot paid two prices");
        assertApproxEqRel(
            fillPerShare, carolPerShare, 1e9, "the fill and a redeem in one slot paid two prices"
        );
        // Nobody leaving is paid at the premium the depositors paid.
        assertLe(bobPerShare, (v.grossExit - v.feeExit) * 1e18 / v.supply, "a leaver was paid the premium");

        // The holder who touched nothing keeps their net price.
        emit log_named_uint("passive holder's net price before the slot", stayerBefore);
        emit log_named_uint("passive holder's net price after the slot", stayerAfter);
        assertGe(stayerAfter + 1, stayerBefore, "a passive holder lost on the slot");
    }

    function tankFilled(Vm.Log[] memory logs) internal pure returns (uint256 shares, uint256 payout) {
        bytes32 topic = keccak256("TankFilled(uint256,uint256)");
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] == topic) return abi.decode(logs[i].data, (uint256, uint256));
        }
    }

    function test_one_slot_one_price_with_a_fee_owed() public {
        oneSlot(false);
    }

    function test_one_slot_one_price_on_payday() public {
        oneSlot(true);
    }
}
