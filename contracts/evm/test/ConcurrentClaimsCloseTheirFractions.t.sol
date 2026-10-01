// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// Two claims served before either is priced each close exactly their
/// leaver's fraction: a claim whose fraction is already home owns none of
/// what is left at the station. With three equal holders and two leaving,
/// the one who stays keeps a third of the positions.
contract ConcurrentClaimsCloseTheirFractionsTest is Fixture {
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

    function keyOfCarol(address who) internal view returns (uint256) {
        return who == carol ? carolKey : keyOf(who);
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

    /// 285k at the venue, one BTC long against one short, no spread.
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

    function test_two_claims_served_before_pricing_each_close_their_fraction() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        depositCarol(100_000e6);
        // 285k deployed; 15k of cash left (the 5% floor of 300k).
        hedge(285_000e6);

        // Alice and bob both leave: neither can be paid from 15k.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        value(vault);
        assertEq(vault.claimCount(), 2);

        // Both are served before the next valuation, each closing a third of
        // the positions.
        blessExecutorKey(executor, address(vault), bundle);
        vault.settleClaim(1);
        vault.settleClaim(2);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);
        assertTrue(vault.claimAt(2).state == ClaimState.Queued);
        assertApproxEqAbs(venue.positionOf(address(station), SPOT).size, 0.333e18, 0.002e18);

        // The next valuation pays both.
        value(vault);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        assertTrue(vault.claimAt(2).state == ClaimState.Paid, "the second claim closed too little");
        int256 left = venue.positionOf(address(station), SPOT).size;
        assertGe(left, 0.33e18, "claims closed more of the pool's positions than the leavers' fractions");
    }
}
