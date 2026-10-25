// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {
    IAggregatorV3,
    IModuleProxyFactory,
    ISafeProxyFactory,
    IUniswapV3Pool
} from "../../src/drivers/robinhood/RobinhoodChain.sol";
import {RobinhoodDriver} from "../../src/drivers/robinhood/RobinhoodDriver.sol";
import {Call} from "../../src/interfaces/IDriver.sol";

import {
    MockFeed, MockPool, MockSafeFactory, MockToken
} from "./AStockCloseRoundsByUnderAUnitPerFigure.t.sol";
import {Test} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// Inside the band, a stock whose feed can't be trusted or whose market has
/// run from its feed blocks a close. At the full band, which every holder
/// shares, nothing does: every holding is sold at any price (the close
/// makes its sale, and says it isn't done until nothing is left to sell).
contract AFullBandStockCloseIsNeverBlockedTest is Test {
    MockToken internal usdg;
    MockToken internal nvda;
    MockFeed internal feed;
    RobinhoodDriver internal driver;
    address internal station = makeAddr("station");

    function setUp() public {
        vm.warp(30 days);
        usdg = new MockToken(6);
        nvda = new MockToken(18);
        MockPool pool = address(usdg) < address(nvda)
            ? new MockPool(address(usdg), address(nvda), 224_390)
            : new MockPool(address(nvda), address(usdg), -224_390);
        feed = new MockFeed(180e8);
        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](1);
        listings[0] = RobinhoodDriver.Listing(
            address(nvda), IAggregatorV3(address(feed)), IUniswapV3Pool(address(pool)), type(uint256).max
        );
        driver = new RobinhoodDriver(
            RobinhoodDriver.Venue({
                dollar: IERC20(address(usdg)),
                router: makeAddr("router"),
                safeSingleton: makeAddr("singleton"),
                safeFactory: ISafeProxyFactory(address(new MockSafeFactory())),
                rolesMastercopy: makeAddr("roles"),
                moduleFactory: IModuleProxyFactory(makeAddr("moduleFactory")),
                maxFeedAge: 4 days,
                twapWindow: 1800
            }),
            listings
        );
        vm.etch(driver.safeOf(station), hex"00");
        nvda.mint(driver.safeOf(station), 100e18);
    }

    function _assertBlockedOnlyInsideTheBand() internal view {
        (, bool blocked) = driver.close(station, 1e18, 500);
        assertTrue(blocked, "inside the band the close went ahead");
        Call[] memory calls;
        (calls, blocked) = driver.close(station, 1e18, 10_000);
        assertEq(calls.length, 2, "a full-band close did not sell");
        assertTrue(blocked, "a full-band close that sells said it was done");
    }

    function test_a_stale_feed_blocks_only_inside_the_band() public {
        feed.set(180e8, 5 days);
        _assertBlockedOnlyInsideTheBand();
    }

    function test_a_feed_with_no_price_blocks_only_inside_the_band() public {
        feed.set(0, 0);
        _assertBlockedOnlyInsideTheBand();
    }

    function test_a_market_far_from_its_feed_blocks_only_inside_the_band() public {
        feed.set(90e8, 0);
        _assertBlockedOnlyInsideTheBand();
    }
}
