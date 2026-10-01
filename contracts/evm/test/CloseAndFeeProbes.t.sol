// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {IVault, VaultState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A close done in one transaction can't be held off by a `keep`, and the
/// holding fee's dilution lowers the profit fee owed as section 6 says.
contract CloseAndFeeProbesTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function hedge(uint256 amount) internal {
        vm.startPrank(executor);
        station.pull(amount);
        station.approveKey("local", executor);
        station.deposit("local", amount);
        OrderRequest[] memory orders = new OrderRequest[](2);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        orders[1] =
            OrderRequest({market: PERP, isBuy: false, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        setPrice(SPOT, PRICE, 0);
    }

    /// Anyone can `keep()` a halted vault whose tank is below its keep level,
    /// which appends a tank-fill request every slot, and `close()` needs
    /// every request settled. Sent as separate transactions, a griefer's
    /// `keep` between the settle and the close stalls it; sent as one
    /// transaction (value, settle, close) it can't be interleaved. Checked,
    /// found sound: not a finding.
    function test_close_cannot_be_stalled_by_keep_when_done_atomically() public {
        depositAs(vault, alice, 100_000e6);
        vm.prank(author);
        vault.setTankKeep(1000e6);
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);

        // A griefer lands a keep between every settle and close.
        for (uint256 i = 0; i < 3; i++) {
            value(vault);
            vault.keep(0, 0, "");
            vm.expectRevert(IVault.Unsettled.selector);
            vault.close();
        }
        // Done in one transaction, the close goes through.
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        vault.settle(type(uint256).max);
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);
    }

    /// The holding fee is synced by minting shares against an unchanged
    /// mark, so the basis the profit fee is measured from (`mark * S`) grows
    /// with every sync, and the profit fee owed shrinks by
    /// `profitBps * mark * Mh`. This is what section 6 specifies, so it is
    /// not a finding; the probe records the size of the effect: five days
    /// of holding fee at 1% a year on a pool 10% above its mark lowers the
    /// profit fee owed by about $2 on $1,500.
    function test_holding_dilution_lowers_the_profit_fee_owed() public {
        depositAs(vault, alice, 100_000e6);
        // Fresh week, then a 10% gain the next day.
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        skip(1 days);
        give(address(station), 10_000e6);
        value(vault);
        uint256 feeBefore = vault.valuationAt(vault.valuationCount() - 1).fee;
        assertApproxEqAbs(feeBefore, 1500e6, 2e6);

        // Five more days inside the same fee week, then one valuation.
        skip(5 days);
        value(vault);
        uint256 feeAfter = vault.valuationAt(vault.valuationCount() - 1).fee;
        emit log_named_uint("profit fee owed before the holding sync", feeBefore);
        emit log_named_uint("profit fee owed after five days of holding sync", feeAfter);
        // 15% of (100k * 1% * 5/365) ~ $2.05.
        assertApproxEqAbs(feeBefore - feeAfter, 2.05e6, 0.1e6);
    }

    function nextMondayAfter(uint256 t) internal pure returns (uint256) {
        uint256 day = t / 1 days;
        return (day - ((day + 3) % 7)) * 1 days + 7 days;
    }
}
