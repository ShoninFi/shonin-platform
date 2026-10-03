// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Claim, ClaimState, IVault} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A serve is judged at the mark, as a claim is: when the station refuses a
/// close because it would pay the holder nothing at the bid, units worth
/// something at the mark wait instead of being cleared for nothing.
contract ServeJudgedAtTheMarkTest is ClaimBase {
    /// Everything the station holds is at the venue: 190k of margin behind
    /// 30 BTC of perp at 60k. Nothing idle.
    function leveredPerp() internal {
        vm.startPrank(executor);
        station.pull(190_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 190_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 30e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        assertEq(usd.balanceOf(address(station)), 0, "the station keeps nothing idle");
    }

    function test_units_worth_something_at_the_mark_wait_when_underwater_at_the_bid() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        leveredPerp();

        // Alice asks to leave. The market sits outside the band for a day,
        // so nothing prices her request and she takes the fallback: half
        // the cash and units for half of what the station holds.
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        vm.prank(operator);
        venue.setPrices(PERP, PRICE, PRICE, PRICE * 5 / 6, PRICE);
        assertTrue(station.outsideBand());
        value(vault); // void
        skip(1 days + 2 * SLOT);
        vm.prank(alice);
        vault.takeProRata(id);
        (uint256 held,) = vault.unitsOf(alice);
        assertApproxEqRel(held, 0.5e18, 0.02e18, "alice holds units for about half the station");

        // The market comes back inside the band, lower: BTC at 53,733 with a
        // 1.8% spread. The account is worth about 2k at the mark and nothing
        // at the bid. The keeper (or anyone) prices a valuation there.
        setPrice(PERP, 53_733e6, 180);
        assertFalse(station.outsideBand(), "the market is inside the band");
        (uint256 atMark, uint256 atExit) = station.value();
        uint256 aliceAtMark = atMark * held / WAD;
        assertGt(aliceAtMark, 500e6, "alice's units are worth something at the mark");
        assertEq(atExit, 0, "the account is under water at the bid");
        value(vault); // priced, inside the band
        int256 perpBefore = venue.positionOf(address(station), PERP).size;

        // Anyone "serves" her in that block.
        vm.prank(bob);
        (bool ok, bytes memory reason) = address(vault).call(abi.encodeCall(IVault.serveProRata, (alice)));

        (uint256 heldAfter, uint256 claimable) = vault.unitsOf(alice);
        assertEq(venue.positionOf(address(station), PERP).size, perpBefore, "something was sold for her");
        emit log_named_uint("alice's units at the mark, before (usd 6dp)", aliceAtMark);
        emit log_named_uint("alice's units after the serve (wei of 1e18)", heldAfter);
        emit log_named_uint("alice's claimable after the serve (usd 6dp)", claimable);
        if (!ok) emit log_named_bytes("serve refused with", reason);

        // A serve that sold nothing and brought nothing home must either be
        // refused or leave her units as they were; it must not clear units
        // worth hundreds of dollars at the mark.
        assertTrue(
            !ok || heldAfter == held || claimable >= aliceAtMark * 9 / 10,
            "units worth something at the mark were cleared for nothing by a close the station refused"
        );
    }
}
