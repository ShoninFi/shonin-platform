// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {Station} from "../src/Station.sol";
import {Call} from "../src/interfaces/IDriver.sol";
import {ILocalVenue} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// The stand-in driver, answering as a venue whose own vaults hold
/// `EQUITY` of the station's money wherever anyone says.
contract ReclaimableDriver is LocalDriver {
    uint256 public constant EQUITY = 5e6;

    constructor(ILocalVenue venue_, IERC20 asset_) LocalDriver(venue_, asset_) {}

    function reclaim(address, address) external pure returns (Call[] memory calls, uint256 equity) {
        return (calls, EQUITY);
    }

    function heldValue(address, address[] calldata held) external pure returns (uint256) {
        return held.length * EQUITY;
    }
}

/// Money the station's account parked in one of the venue's own vaults is
/// misbehaviour by whoever trades it, and anyone may answer it by reclaiming
/// the money: that ends trading through the driver for good. The parked
/// money still counts in the station's value, and while more than a dollar
/// of it is parked no fraction is closed for anyone.
contract AReclaimEndsTradingThroughTheDriverForGoodTest is ClaimBase {
    address internal stranger = makeAddr("stranger");

    function test_a_strangers_reclaim_ends_trading_through_the_driver_and_parked_money_holds_closes()
        public
    {
        depositAs(vault, alice, 1000e6);
        deploySpot(500e6, 400e6, 0.005e8);
        pinnedDriverActsAs(address(new ReclaimableDriver(venue, IERC20(address(usd)))));

        assertFalse(station.reclaimed("local"));
        vm.prank(stranger);
        station.reclaim("local", makeAddr("some venue vault"));
        assertTrue(station.reclaimed("local"), "one stranger's call set reclaimed");

        // Trading through the driver is over: no key can be approved again,
        // no deposit made.
        vm.startPrank(executor);
        vm.expectRevert(Station.NotAllowed.selector);
        station.approveKey("local", executor);
        vm.expectRevert(Station.NotApproved.selector);
        station.deposit("local", 1e6);
        vm.stopPrank();
        // And it can't be undone by anyone.
        assertTrue(station.reclaimed("local"));

        // The vault still values (the parked money counts) ...
        value(vault);
        (bool priced,,) = vault.lastPriced();
        assertTrue(priced);
        // ... but while the "parked" figure is above a dollar every
        // fractional close is blocked: a claim waits.
        uint256 __s_alice = vault.balanceOf(alice);
        vm.prank(alice);
        vault.requestRedeem(__s_alice, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);
        vault.settleClaim(1);
        assertEq(uint256(vault.claimAt(1).state), uint256(ClaimState.Waiting), "claim blocked by parked");
    }
}
