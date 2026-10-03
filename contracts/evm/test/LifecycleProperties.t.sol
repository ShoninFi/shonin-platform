// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {
    ClaimState,
    IVault,
    Request,
    RequestKind,
    ValuationStatus,
    VaultState
} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Properties of the lifecycle paths: sequences of several calls by
/// different parties across a halt, the stopped station's widening band, the
/// close and the closed vault's doors. Each asserts the intended rule; a
/// failure here is a defect.
contract LifecyclePropertiesTest is ClaimBase {
    function _only(bytes32 name) internal pure returns (bytes32[] memory names) {
        names = new bytes32[](1);
        names[0] = name;
    }

    function _nextSlot() internal returns (uint64 time) {
        uint256 t = vm.getBlockTimestamp();
        time = uint64(t - (t % SLOT) + SLOT);
        vm.warp(time);
    }

    function _balanceCovers() internal view {
        (, uint256 reserve) = vault.units();
        assertGe(
            usd.balanceOf(address(vault)),
            vault.cash() + vault.tank().balance + vault.escrow() + reserve + _heldAll(),
            "the vault's balance no longer covers what it owes"
        );
    }

    function _heldAll() internal view returns (uint256 total) {
        total = vault.heldFor(alice) + vault.heldFor(bob) + vault.heldFor(carol) + vault.heldFor(author)
            + vault.heldFor(platform);
    }

    /// A deposit paid by one person for another is filed while Open, just
    /// before a kill, so it is still unpriced when the vault halts; the next
    /// valuation refunds it at settlement (the vault is no longer Open), to
    /// the payer and not to the named receiver, and its fallback door is
    /// then shut.
    function test_p1_a_deposit_priced_after_the_halt_goes_back_to_the_payer_once() public {
        depositAs(vault, alice, 50_000e6);
        // Carol pays for a deposit in bob's name.
        bytes memory statement = statementBy(bobKey, address(vault), bob, vault.restrictedText());
        give(carol, 1000e6);
        vm.startPrank(carol);
        usd.approve(address(vault), 1000e6);
        uint256 id = vault.requestDeposit(1000e6, bob, statement);
        vm.stopPrank();
        assertEq(usd.balanceOf(carol), 0);

        vm.prank(admin);
        vault.kill();
        _nextSlot();
        vault.recordValuation();
        vault.settle(type(uint256).max);
        assertTrue(vault.requestAt(id).done, "the deposit wasn't settled");
        assertEq(usd.balanceOf(carol), 1000e6, "the refund didn't go to the payer");
        assertEq(usd.balanceOf(bob), 0, "the refund went to the named receiver");
        assertEq(vault.balanceOf(bob), 0, "the deposit bought shares in a halted vault");
        assertEq(vault.escrow(), 0);
        _balanceCovers();

        // Its fallback door is shut too: it is done.
        vm.prank(bob);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.takeProRata(id);
    }

    /// A claim blocked by the ordinary band on a stopped station waits; the
    /// band widens over the weeks until a stranger's full unwind sells
    /// everything; the claim is then covered by the cash, queued, and paid
    /// at the empty valuation's price; the vault closes and every door adds
    /// up. Meanwhile the author's manual tank fill and a stranger's `keep`
    /// fill ride along without holding anything up.
    function test_p2_a_waiting_claim_is_paid_once_the_widened_band_brings_everything_home() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        // 190k to the station, 150k of it at the venue, 2 BTC spot (120k).
        deploySpot(190_000e6, 150_000e6, 2e18);
        value(vault);

        // Bob asks to leave: the cash (10k) can't pay him, so a claim.
        uint256 claimId = claimFor(bob, bobShares);

        // The bid drops 5% under the venue's own price and stays there.
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 95 / 100, PRICE);
        vault.settleClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Waiting, "the claim wasn't blocked");

        // A week on, anyone freezes the vault; after the grace the station
        // is stopped; the ordinary band still refuses the claim's close.
        skip(7 days);
        vault.freeze(claimId);
        assertTrue(vault.state() == VaultState.Frozen);
        skip(HALT_GRACE);
        station.stop();
        vault.settleClaim(claimId);
        assertTrue(
            vault.claimAt(claimId).state == ClaimState.Waiting, "the stopped station closed inside the band"
        );

        // The author wants the tank topped up by hand, and a stranger keeps
        // the vault; both fills queue and roll on through void valuations.
        vm.prank(author);
        vault.setTankKeep(1000e6);
        vm.prank(author);
        vault.topUpFromShares(1);
        vm.prank(carol);
        vault.keep(0, 0, "");

        // Weeks two and three: the band is 2% then 4%; the 5% market is still
        // outside it, so a full unwind brings only the idle dollars home.
        skip(7 days);
        vm.prank(carol);
        (uint256 sentHome, bool blocked) = station.unwindStopped(_only("local"));
        assertTrue(blocked, "a 2% band reached a 5% market");
        assertEq(sentHome, 40_000e6, "the idle dollars didn't come home");
        assertEq(spotSize(), 2e18, "the position was sold outside the band");

        // Week three: 4%: still outside.
        skip(7 days);
        vm.prank(carol);
        (sentHome, blocked) = station.unwindStopped(_only("local"));
        assertTrue(blocked, "a 4% band reached a 5% market");
        assertEq(spotSize(), 2e18);
        // Week four: 8%: everything sells at the 5% discount.
        skip(7 days);
        vm.prank(carol);
        (sentHome, blocked) = station.unwindStopped(_only("local"));
        assertFalse(blocked, "the widened band didn't reach the market");
        assertEq(spotSize(), 0);
        assertEq(usd.balanceOf(address(station)), 0);
        assertEq(uint256(venue.cashOf(address(station))), 0, "money was left at the venue");
        _balanceCovers();

        // The cash now covers the claim at the last priced valuation's exit
        // price: it is queued and priced at the next valuation.
        vault.settleClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Queued, "a covered claim wasn't queued");
        // Nothing may go out to the station meanwhile (and it couldn't anyway).
        _nextSlot();
        vault.recordValuation();
        assertTrue(latest().status == ValuationStatus.Priced, "an empty station priced nothing");
        vault.settle(type(uint256).max);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Paid, "the queued claim wasn't paid");
        uint256 bobPaid = usd.balanceOf(bob);
        assertGt(bobPaid, 90_000e6, "bob was paid less than his share of a 5% fall");
        assertLt(bobPaid, 98_000e6, "bob was paid more than his share");
        _balanceCovers();

        // The fills were settled too: the tank grew from the author's shares.
        assertFalse(vault.tank().fillPending, "the keep fill is still pending");

        // Close, and alice takes hers.
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);
        vm.prank(alice);
        uint256 alicePaid = vault.redeemClosed(aliceShares, alice);
        assertApproxEqRel(alicePaid, bobPaid, 0.03e18, "the stayer and the leaver bore different falls");
        for (uint256 i = 0; i < 2; i++) {
            address holder = i == 0 ? platform : author;
            uint256 shares = vault.balanceOf(holder);
            if (shares == 0) continue;
            vm.prank(holder);
            vault.redeemClosed(shares, holder);
        }
        assertEq(vault.totalSupply(), 0, "shares outlived every door");
        assertLe(vault.cash(), 4, "money was stranded");
        _balanceCovers();
    }

    /// A claim the cash covers is queued; before its slot is priced the
    /// vault is frozen on its clock (a week from booking); the next
    /// valuation still pays it; the vault then stops, unwinds, closes, and
    /// the paid claim's closed-vault door refuses a second payment.
    function test_p3_a_queued_claim_is_paid_after_a_freeze_and_never_twice() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 150_000e6, 2e18);
        value(vault);
        uint256 claimId = claimFor(bob, bobShares);
        uint64 booked = vault.claimAt(claimId).bookedAt;

        // The executor brings cash home (the strategy deleverages), then a
        // valuation: the claim is now covered.
        vm.startPrank(executor);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] = OrderRequest({market: SPOT, isBuy: false, size: 2e18, limitPrice: 0, reduceOnly: true});
        venue.place(address(station), orders);
        station.withdraw("local", 150_000e6);
        station.push(190_000e6);
        vm.stopPrank();
        value(vault);
        vault.settleClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Queued);

        // A week from booking, before its slot: anyone freezes.
        vm.warp(booked + 7 days);
        vault.freeze(claimId);
        assertTrue(vault.state() == VaultState.Frozen);
        // The queued request is still priced and paid.
        value(vault);
        assertTrue(
            vault.claimAt(claimId).state == ClaimState.Paid, "the queued claim wasn't paid after the freeze"
        );
        uint256 bobPaid = usd.balanceOf(bob);
        assertGt(bobPaid, 99_000e6);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.settleClaim(claimId);

        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);
        value(vault);
        vault.close();
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.claimClosed(claimId);
        vm.prank(alice);
        uint256 alicePaid = vault.redeemClosed(aliceShares, alice);
        assertApproxEqRel(alicePaid, bobPaid, 0.005e18, "one price for the leaver and the stayer");
        assertEq(usd.balanceOf(bob), bobPaid, "bob was paid again");
        _balanceCovers();
    }

    /// After the close, `settle` by anyone changes nothing, a manual tank
    /// fill filed after the last valuation lapses through `settleClosed`,
    /// and the author's shares are theirs to redeem at the closed price.
    function test_p4_settle_after_the_close_is_a_no_op_and_a_late_manual_fill_lapses() public {
        depositAs(vault, alice, 100_000e6);
        // The author holds some shares.
        depositAs(vault, author, 10_000e6);
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);

        // Just before the empty valuation's slot: a manual fill from shares.
        uint64 slot = _nextSlot();
        vm.warp(slot - 4);
        vm.prank(author);
        vault.topUpFromShares(5000e18);
        uint256 fillId = vault.requestCount();
        assertGt(vault.requestAt(fillId).earliest, slot);
        vm.warp(slot);
        vault.recordValuation();
        vault.settle(type(uint256).max);
        vault.close();

        uint256 cashBefore = vault.cash();
        uint256 supplyBefore = vault.totalSupply();
        uint256 tankBefore = vault.tank().balance;
        uint256 authorBefore = vault.balanceOf(author);
        vault.settle(type(uint256).max);
        assertEq(vault.cash(), cashBefore, "settle after the close moved cash");
        assertEq(vault.totalSupply(), supplyBefore, "settle after the close moved shares");
        assertFalse(vault.requestAt(fillId).done, "settle after the close settled a request");

        vault.settleClosed(fillId);
        assertTrue(vault.requestAt(fillId).done);
        assertEq(vault.tank().balance, tankBefore, "a lapsed fill filled the tank");
        assertEq(vault.balanceOf(author), authorBefore, "the author lost shares to a lapsed fill");
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.settleClosed(fillId);

        uint256 authorShares = vault.balanceOf(author);
        vm.prank(author);
        uint256 paid = vault.redeemClosed(authorShares, author);
        assertApproxEqRel(paid, 10_000e6, 0.01e18);
        _balanceCovers();
    }
}
