// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState, IVault, Valuation} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";
import {Vm} from "forge-std/Vm.sol";

/// A claim whose fraction has been closed pays its own fees and no one
/// else's: its profit fee is owed against the mark when it was queued,
/// whether or not a payday raises the mark before it is priced, and no
/// holding fee is minted on its shares for the stayers to pay.
contract ClosedClaimFeesTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function nextMondayAfter(uint256 t) internal pure returns (uint256) {
        uint256 day = t / 1 days;
        return (day - ((day + 3) % 7)) * 1 days + 7 days;
    }

    function latest() internal view returns (Valuation memory) {
        return vault.valuationAt(vault.valuationCount() - 1);
    }

    /// Two equal holders; 190k pulled, three BTC bought at 60k, 10k idle at
    /// the venue; then spot rises 20%, so the pool is above its mark. Alice's
    /// withdrawal becomes a claim (10k of cash can't pay ~118k) and her half
    /// is closed and set aside. Returns what her shares own by the rule.
    function aliceClosedAboveTheMark()
        internal
        returns (uint256 aliceShares, uint256 owned, uint256 feeOwed)
    {
        // A fresh fee week, so the next payday is next Monday.
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);

        vm.startPrank(executor);
        station.pull(190_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 190_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 3e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        setPrice(SPOT, PRICE * 12 / 10, 0);
        value(vault);
        assertGt(latest().fee, 0, "the pool is above its mark, so a fee is owed");

        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);
        assertTrue(vault.claimAt(1).state == ClaimState.Pending);

        uint256 cashBefore = vault.cash();
        uint256 supply = vault.totalSupply();
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);
        uint256 home = vault.cash() - cashBefore;
        // `cashShare` exactly as `settleClaim` computes it (no units, no
        // closed shares yet).
        uint256 cashShare = cashBefore * aliceShares / supply;
        owned = home + cashShare;
        // Her own profit fee, measured on what she owns against the mark her
        // shares were last measured at.
        uint256 basis = vault.mark() * aliceShares / 1e30;
        feeOwed = (owned - basis) * 1500 / 10_000;
        assertGt(feeOwed, 1000e6, "the claim owes a fee worth testing");
    }

    function test_a_closed_claim_priced_at_a_payday_valuation_pays_the_fee_it_owes() public {
        (, uint256 owned, uint256 feeOwed) = aliceClosedAboveTheMark();
        uint256 markBefore = vault.mark();

        // Payday arrives before the claim is priced.
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        assertGt(vault.mark(), markBefore, "payday raised the mark");

        uint256 paid = vault.claimAt(1).paid;
        uint256 gross = owned - feeOwed;
        uint256 expectedPaid = gross - (gross * 10 + 9999) / 10_000;
        emit log_named_uint("alice owns", owned);
        emit log_named_uint("profit fee she owes", feeOwed);
        emit log_named_uint("she should be paid", expectedPaid);
        emit log_named_uint("she was paid", paid);
        // A week of holding fee moves nothing here: her amount is fixed and
        // the mark isn't synced by it. The tolerance covers rounding only.
        assertLe(
            paid,
            expectedPaid + 50e6,
            "a closed claim priced at a payday valuation was paid most of the profit fee it owed"
        );
    }

    /// The same claim priced at an ordinary valuation pays the same fee.
    function test_a_closed_claim_priced_at_an_ordinary_valuation_pays_the_fee_it_owes() public {
        (, uint256 owned, uint256 feeOwed) = aliceClosedAboveTheMark();
        value(vault);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        uint256 paid = vault.claimAt(1).paid;
        uint256 gross = owned - feeOwed;
        uint256 expectedPaid = gross - (gross * 10 + 9999) / 10_000;
        emit log_named_uint("she should be paid", expectedPaid);
        emit log_named_uint("she was paid", paid);
        assertApproxEqAbs(paid, expectedPaid, 1e6, "priced at an ordinary valuation, the claim pays its fee");
    }

    function test_the_holding_fee_is_not_minted_on_a_closed_claims_shares() public {
        (uint256 aliceShares,,) = aliceClosedAboveTheMark();
        uint256 supplyWithOffset = vault.totalSupply() + 1e12;
        uint64 lastValued = latest().time;

        // A day passes before the next valuation.
        skip(1 days);
        vm.recordLogs();
        value(vault);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 minted;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] == IVault.HoldingSynced.selector) {
                minted = abi.decode(logs[i].data, (uint256));
            }
        }
        uint256 elapsed = latest().time - lastValued;
        uint256 rate = uint256(100) * elapsed * 1e18 / (10_000 * 365 days);
        uint256 onStayers = (supplyWithOffset - aliceShares) * rate / (1e18 - rate);
        uint256 onEveryone = supplyWithOffset * rate / (1e18 - rate);
        emit log_named_uint("holding fee minted", minted);
        emit log_named_uint("on the stayers' shares alone", onStayers);
        emit log_named_uint("on every share, closed ones included", onEveryone);
        assertLe(
            minted,
            onStayers + 1e12,
            "the holding fee was minted on the closed claim's shares too, and the stayers pay it"
        );
    }
}
