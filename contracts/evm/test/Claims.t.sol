// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState, IVault} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A withdrawal the cash can't pay becomes a claim. Serving it closes the
/// leaver's fraction of every position, and pays them in the same
/// transaction: what the close brought home and their part of the cash as it
/// stood just before, less their own profit fee (paid in cash to the author
/// and the platform) and the transaction fee (kept in the pool). Their
/// shares are burned there and then. Nothing anyone does afterwards can
/// change what they were paid, and the holders who stay are left owning
/// exactly what they owned, at the same price per share or better.
contract ClaimsTest is Fixture {
    uint256 internal constant WAD = 1e18;

    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;
    address internal carol;
    uint256 internal carolKey;

    function setUp() public override {
        super.setUp();
        (carol, carolKey) = makeAddrAndKey("carol");
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    // Helpers ---------------------------------------------------------------

    function depositCarol(uint256 amount) internal returns (uint256 shares) {
        bytes memory statement = statementBy(carolKey, address(vault), carol, vault.restrictedText());
        give(carol, amount);
        vm.startPrank(carol);
        usd.approve(address(vault), amount);
        vault.requestDeposit(amount, carol, statement);
        vm.stopPrank();
        value(vault);
        shares = vault.balanceOf(carol);
    }

    /// `amount` to the venue as `size` BTC spot long against the same perp
    /// short, at `PRICE` with no spread.
    function hedge(uint256 amount, uint256 size) internal {
        vm.startPrank(executor);
        station.pull(amount);
        station.approveKey("local", executor);
        station.deposit("local", amount);
        OrderRequest[] memory orders = new OrderRequest[](2);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: size, limitPrice: PRICE, reduceOnly: false});
        orders[1] =
            OrderRequest({market: PERP, isBuy: false, size: size, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
    }

    /// What `shares` own right now: their part of the cash and of what the
    /// shares own of the station, at exit prices.
    function worth(uint256 shares) internal view returns (uint256) {
        (, uint256 exit) = station.value();
        (uint256 units,) = vault.units();
        uint256 owned = vault.cash() + exit * (WAD - units) / WAD;
        return owned * shares / vault.totalSupply();
    }

    function spot() internal view returns (uint256) {
        return uint256(venue.positionOf(address(station), SPOT).size);
    }

    /// Ask to redeem every share, have it priced, and return the claim it
    /// became.
    function claimFor(address who, uint256 shares) internal returns (uint256 claimId) {
        vm.prank(who);
        vault.requestRedeem(shares, who);
        value(vault);
        claimId = vault.claimCount();
        assertTrue(vault.claimAt(claimId).state == ClaimState.Pending, "it didn't become a claim");
    }

    // Paid at the close -----------------------------------------------------

    function test_a_claim_is_paid_in_the_transaction_that_brings_its_fraction_home() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        hedge(190_000e6, 1e18);
        uint256 id = claimFor(alice, aliceShares);
        uint256 aliceWorth = worth(aliceShares);
        uint256 bobBefore = worth(bobShares);

        vault.settleClaim(id);

        assertTrue(vault.claimAt(id).state == ClaimState.Paid, "not paid at its close");
        assertApproxEqAbs(spot(), 0.5e18, 0.001e18, "her half of the position was closed");
        // Nothing above the mark: only the transaction fee comes off.
        uint256 paid = usd.balanceOf(alice);
        assertEq(paid, vault.claimAt(id).paid);
        assertLe(paid, aliceWorth, "she was paid more than her shares owned");
        assertApproxEqRel(paid, aliceWorth * 999 / 1000, 0.0005e18);
        // The vault holds only holding-fee shares waiting for payday.
        assertLt(vault.balanceOf(address(vault)), aliceShares / 10_000, "her shares weren't burned");
        // Bob owns what he owned, and a share of the fee she left behind.
        assertGe(worth(bobShares) + 1, bobBefore, "the holder who stayed paid for her exit");
    }

    function test_a_paid_claim_cannot_be_tried_again_and_nothing_closes_twice() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        hedge(190_000e6, 1e18);
        uint256 id = claimFor(alice, aliceShares);
        vault.settleClaim(id);
        uint256 left = spot();

        vm.expectRevert(IVault.NotAllowed.selector);
        vault.settleClaim(id);
        value(vault);
        assertEq(spot(), left, "a paid claim closed more");
    }

    function test_two_claims_served_one_after_the_other_each_take_their_fraction() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        uint256 carolShares = depositCarol(100_000e6);
        hedge(285_000e6, 1e18);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        value(vault);
        assertEq(vault.claimCount(), 2);
        uint256 carolBefore = worth(carolShares);

        uint256 aliceWorth = worth(aliceShares);
        vault.settleClaim(1);
        uint256 bobWorth = worth(bobShares);
        vault.settleClaim(2);

        assertApproxEqRel(usd.balanceOf(alice), aliceWorth * 999 / 1000, 0.0005e18);
        assertApproxEqRel(usd.balanceOf(bob), bobWorth * 999 / 1000, 0.0005e18);
        assertApproxEqAbs(
            spot(), uint256(1e18) / 3, 0.002e18, "the claims closed more or less than two thirds"
        );
        assertGe(worth(carolShares) + 1, carolBefore, "the holder who stayed paid for their exits");
    }

    // Fees ------------------------------------------------------------------

    function test_a_claim_pays_its_own_profit_fee_in_cash_and_nobody_elses() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        hedge(190_000e6, 1e18);
        // A gain at the station puts the pool above its mark.
        give(address(station), 20_000e6);
        uint256 id = claimFor(alice, aliceShares);
        uint256 aliceWorth = worth(aliceShares);
        uint256 basis = vault.mark() * aliceShares / 1e30;
        uint256 fee = (aliceWorth - basis) * 1500 / 10_000;
        uint256 bobBefore = worth(bobShares);
        uint256 authorBefore = usd.balanceOf(author);
        uint256 platformBefore = usd.balanceOf(platform);

        vault.settleClaim(id);

        uint256 toAuthor = usd.balanceOf(author) - authorBefore;
        uint256 toPlatform = usd.balanceOf(platform) - platformBefore;
        assertApproxEqRel(toAuthor + toPlatform, fee, 0.001e18, "her fee wasn't what her shares owed");
        assertApproxEqRel(toAuthor, toPlatform * 2, 0.001e18, "the fee wasn't split ten to five");
        uint256 gross = aliceWorth - fee;
        assertApproxEqRel(usd.balanceOf(alice), gross - gross / 1000, 0.0005e18);
        assertGe(worth(bobShares) + 1, bobBefore, "the holder who stayed paid her fee");
    }

    function test_a_payday_after_the_close_changes_nothing_she_was_paid() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        hedge(190_000e6, 1e18);
        give(address(station), 20_000e6);
        uint256 id = claimFor(alice, aliceShares);
        vault.settleClaim(id);
        uint256 paid = usd.balanceOf(alice);
        skip(8 days);
        value(vault);
        assertEq(usd.balanceOf(alice), paid);
        assertEq(vault.claimAt(id).paid, paid);
    }

    // With fallback units outstanding ----------------------------------------

    function test_a_claim_served_while_units_exist_takes_all_its_close_and_the_units_keep_their_value()
        public
    {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        uint256 carolShares = depositCarol(100_000e6);
        vm.prank(executor);
        station.pull(285_000e6);

        // Carol leaves through the fallback: units for a third of the station.
        vm.prank(carol);
        uint256 carolRequest = vault.requestRedeem(carolShares, carol);
        skip(24 hours + SLOT);
        vm.prank(carol);
        vault.takeProRata(carolRequest);
        (uint256 carolUnits,) = vault.unitsOf(carol);
        (, uint256 exitBefore) = station.value();
        uint256 carolUnitsWorth = exitBefore * carolUnits / WAD;

        uint256 id = claimFor(alice, aliceShares);
        uint256 aliceWorth = worth(aliceShares);
        uint256 bobBefore = worth(bobShares);
        vault.settleClaim(id);

        assertTrue(vault.claimAt(id).state == ClaimState.Paid);
        assertApproxEqRel(usd.balanceOf(alice), aliceWorth * 999 / 1000, 0.0005e18, "the units took a cut");
        (uint256 carolUnitsAfter,) = vault.unitsOf(carol);
        (, uint256 exitAfter) = station.value();
        assertApproxEqRel(
            exitAfter * carolUnitsAfter / WAD, carolUnitsWorth, 0.0001e18, "the units lost value"
        );
        assertGe(worth(bobShares) + 1e6, bobBefore, "the holder who stayed paid for her exit");
    }

    // Claims the cash covers ------------------------------------------------

    function test_a_claim_the_cash_covers_is_priced_at_the_next_valuation_and_its_cash_stays_home() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        vm.prank(executor);
        station.pull(190_000e6);
        uint256 id = claimFor(alice, aliceShares);
        vm.prank(executor);
        station.push(190_000e6);

        // Nothing to close: an ordinary request, priced at the next slot.
        vault.settleClaim(id);
        assertTrue(vault.claimAt(id).state == ClaimState.Queued);
        assertEq(usd.balanceOf(alice), 0, "paid before it was priced");
        // Meanwhile nothing goes out to the station.
        vm.prank(executor);
        vm.expectRevert(IVault.WrongState.selector);
        station.pull(1);

        value(vault);
        assertTrue(vault.claimAt(id).state == ClaimState.Paid);
        assertApproxEqRel(usd.balanceOf(alice), 100_000e6 * 999 / 1000, 0.001e18);
        vm.prank(executor);
        station.pull(1);
    }

    // Forward pricing holds around a claim -----------------------------------

    function test_a_deposit_beside_the_leavers_own_claim_buys_at_the_next_valuation_and_takes_nothing()
        public
    {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        uint256 carolShares = depositCarol(100_000e6);
        hedge(285_000e6, 1e18);
        uint256 id = claimFor(alice, aliceShares);
        uint256 aliceWorth = worth(aliceShares);
        uint256 bobBefore = worth(bobShares);
        uint256 carolBefore = worth(carolShares);

        // Alice asks to deposit again, then serves her own claim.
        requestDepositAs(vault, alice, 100_000e6);
        vault.settleClaim(id);
        assertLe(usd.balanceOf(alice), aliceWorth, "her claim took part of the deposit");
        value(vault);

        // Her new shares cost what they are worth; nobody else lost.
        assertApproxEqRel(worth(vault.balanceOf(alice)), 100_000e6 * 999 / 1000, 0.001e18);
        assertGe(worth(bobShares) + 1e6, bobBefore);
        assertGe(worth(carolShares) + 1e6, carolBefore);
    }

    // The freeze clock ------------------------------------------------------

    function test_a_paid_claim_can_never_freeze_the_vault() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        hedge(190_000e6, 1e18);
        uint256 id = claimFor(alice, aliceShares);
        skip(7 days - 1);
        vault.settleClaim(id);
        skip(1 days);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.freeze(id);
    }
}
