// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState, Valuation} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// Shared helpers for the claim tests: a vault with a third depositor, a
/// way to put the station into a spot position with a chosen spread, and
/// "what do these shares own right now" measured directly from the cash and
/// the station's exit value rather than from any valuation.
abstract contract ClaimBase is Fixture {
    uint256 internal constant WAD = 1e18;

    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;
    address internal carol;
    uint256 internal carolKey;

    function setUp() public virtual override {
        super.setUp();
        (carol, carolKey) = makeAddrAndKey("carol");
        (vault, bundle) = openVault();
        station = stationOf(vault);
        // The stand-in venue is every trade's counterparty: give it the
        // reserves a real venue's other traders would be, so a gain can
        // always be withdrawn.
        give(address(venue), 10_000_000e6);
    }

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

    /// Pull `amount` to the station, put `atVenue` of it at the venue and buy
    /// `size` BTC spot there at PRICE with no spread. The rest of `amount`
    /// stays as the station's idle dollars.
    function deploySpot(uint256 amount, uint256 atVenue, uint256 size) internal {
        vm.startPrank(executor);
        station.pull(amount);
        station.approveKey("local", executor);
        if (atVenue != 0) station.deposit("local", atVenue);
        if (size != 0) {
            OrderRequest[] memory orders = new OrderRequest[](1);
            orders[0] =
                OrderRequest({market: SPOT, isBuy: true, size: size, limitPrice: PRICE, reduceOnly: false});
            venue.place(address(station), orders);
        }
        vm.stopPrank();
    }

    /// What `shares` own right now at exit prices: their part of the cash
    /// and of the shares' part of the station.
    function worth(uint256 shares) internal view returns (uint256) {
        (, uint256 exit) = station.value();
        (uint256 units,) = vault.units();
        uint256 owned = vault.cash() + exit * (WAD - units) / WAD;
        return owned * shares / vault.totalSupply();
    }

    /// Per-share worth at exit prices, scaled by 1e18 so dust compares.
    function perShare() internal view returns (uint256) {
        (, uint256 exit) = station.value();
        (uint256 units,) = vault.units();
        uint256 owned = vault.cash() + exit * (WAD - units) / WAD;
        return owned * 1e18 / vault.totalSupply();
    }

    function spotSize() internal view returns (uint256) {
        return uint256(venue.positionOf(address(station), SPOT).size);
    }

    function latest() internal view returns (Valuation memory) {
        return vault.valuationAt(vault.valuationCount() - 1);
    }

    function claimFor(address who, uint256 shares) internal returns (uint256 claimId) {
        vm.prank(who);
        vault.requestRedeem(shares, who);
        value(vault);
        claimId = vault.claimCount();
        assertTrue(vault.claimAt(claimId).state == ClaimState.Pending, "it didn't become a claim");
    }

    function nextMondayAfter(uint256 t) internal pure returns (uint256) {
        uint256 day = t / 1 days;
        return (day - ((day + 3) % 7)) * 1 days + 7 days;
    }

    function ceilDiv(uint256 a, uint256 b) internal pure returns (uint256) {
        return (a + b - 1) / b;
    }
}
