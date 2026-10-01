// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalBridge} from "../src/LocalBridge.sol";
import {LocalDriver} from "../src/LocalDriver.sol";
import {LocalUsd} from "../src/LocalUsd.sol";
import {LocalVenue} from "../src/LocalVenue.sol";
import {Call} from "../src/interfaces/IDriver.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Test} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// Makes the calls it is handed, as a station does once it has checked them.
contract Runner {
    function run(Call[] memory calls) external {
        for (uint256 i = 0; i < calls.length; i++) {
            (bool ok, bytes memory reason) = calls[i].target.call{value: calls[i].value}(calls[i].data);
            if (!ok) {
                assembly {
                    revert(add(reason, 32), mload(reason))
                }
            }
        }
    }
}

contract LocalDriverTest is Test {
    bytes32 internal constant SPOT = "BTC";
    bytes32 internal constant PERP = "BTC-PERP";
    uint256 internal constant PRICE = 60_000e6;

    address internal operator = makeAddr("operator");
    address internal key = makeAddr("key");

    LocalUsd internal usd;
    LocalVenue internal venue;
    LocalDriver internal driver;
    LocalBridge internal bridge;
    Runner internal station;

    function setUp() public {
        usd = new LocalUsd();
        venue = new LocalVenue(usd, operator, 1000);
        driver = new LocalDriver(venue, IERC20(address(usd)));
        bridge = new LocalBridge(IERC20(address(usd)), operator);
        station = new Runner();
        vm.startPrank(operator);
        venue.listMarket(SPOT, false, PRICE);
        venue.listMarket(PERP, true, PRICE);
        vm.stopPrank();
        usd.mint(address(station), 100_000e6);
    }

    function setPrice(bytes32 market, uint256 mark, uint256 bid, uint256 ask) internal {
        vm.prank(operator);
        venue.setPrices(market, mark, mark, bid, ask);
    }

    /// The account holds one BTC long in spot and one BTC short in the perp.
    function hedged() internal {
        station.run(driver.deposit(address(station), 100_000e6));
        station.run(driver.approveKey(address(station), key, uint64(block.timestamp + 1 days)));
        OrderRequest[] memory orders = new OrderRequest[](2);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        orders[1] =
            OrderRequest({market: PERP, isBuy: false, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        vm.prank(key);
        venue.place(address(station), orders);
    }

    function test_it_describes_itself_and_reaches_only_the_venue() public view {
        assertEq(driver.name(), "local");
        assertEq(driver.targets().length, 1);
        assertEq(driver.targets()[0], address(venue));
        assertEq(driver.restricted().length, 0);
    }

    function test_the_station_is_the_master_and_a_key_only_trades() public {
        hedged();
        assertTrue(venue.isAgent(address(station), key));
        (uint256 mark,) = driver.valueOf(address(station));
        assertEq(mark, 100_000e6);
        vm.prank(key);
        vm.expectRevert();
        venue.withdraw(1);
        station.run(driver.revokeKey(address(station), key));
        assertFalse(venue.isAgent(address(station), key));
    }

    function test_a_close_takes_the_fraction_of_every_position_and_a_sweep_brings_its_share_home() public {
        hedged();
        (Call[] memory calls, bool blocked) = driver.close(address(station), 0.25e18, 100);
        assertFalse(blocked);
        station.run(calls);
        assertEq(venue.positionOf(address(station), SPOT).size, 0.75e18);
        assertEq(venue.positionOf(address(station), PERP).size, -0.75e18);

        uint256 before = usd.balanceOf(address(station));
        station.run(driver.sweep(address(station), 0.25e18));
        assertEq(usd.balanceOf(address(station)) - before, 25_000e6);
    }

    function test_nothing_is_closed_when_a_market_is_outside_the_band() public {
        hedged();
        // The spot bid has fallen two per cent below the venue's price.
        setPrice(SPOT, PRICE, (PRICE * 98) / 100, PRICE);
        (Call[] memory calls, bool blocked) = driver.close(address(station), 0.5e18, 100);
        assertTrue(blocked);
        assertEq(calls.length, 0);
    }

    function test_closing_everything_leaves_the_account_flat_and_all_of_it_comes_home() public {
        hedged();
        (Call[] memory calls,) = driver.close(address(station), 1e18, 100);
        station.run(calls);
        station.run(driver.sweep(address(station), 1e18));
        assertEq(venue.positionOf(address(station), SPOT).size, 0);
        assertEq(venue.positionOf(address(station), PERP).size, 0);
        assertEq(usd.balanceOf(address(station)), 100_000e6);
    }

    function test_a_bridge_send_lands_once() public {
        station.run(bridge.send(address(station), 31_338, address(station), 1000e6, ""));
        assertEq(usd.balanceOf(address(bridge)), 1000e6);
        vm.prank(operator);
        bridge.deliver(31_337, 1, address(station), 1000e6);
        vm.prank(operator);
        vm.expectRevert(LocalBridge.AlreadyDelivered.selector);
        bridge.deliver(31_337, 1, address(station), 1000e6);
        vm.expectRevert(LocalBridge.NotOperator.selector);
        bridge.deliver(31_337, 2, address(station), 1000e6);
    }
}
