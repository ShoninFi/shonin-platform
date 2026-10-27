// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {CloseBand} from "../../src/drivers/CloseBand.sol";
import {
    IAggregatorV3,
    IModuleProxyFactory,
    ISafeProxyFactory,
    ISwapRouter02,
    IUniswapV3Pool
} from "../../src/drivers/robinhood/RobinhoodChain.sol";
import {RobinhoodDriver} from "../../src/drivers/robinhood/RobinhoodDriver.sol";
import {UniswapPrice} from "../../src/drivers/robinhood/UniswapPrice.sol";
import {Call} from "../../src/interfaces/IDriver.sol";
import {SkewedPool} from "./AFullBandSaleLimitIsAnchoredToTheTimeWeightedPrice.t.sol";
import {MockFeed, MockSafeFactory, MockToken} from "./AStockCloseRoundsByUnderAUnitPerFigure.t.sol";
import {Test} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// A stopped station's sale, at any band, is one rule: at most `maxSale`
/// a close, no minimum out, and a price limit at whichever is nearer the
/// pool's price of the time-weighted bound (`SALE_SPACINGS` past the nearer
/// of the pool's tick and its time-weighted tick) and the feed's latest
/// answer less the stopped discount. The discount is the band, at most
/// `FULL_BAND_FEED_BPS` (10%), until the band is full; from the full band
/// it doubles each week (20%, 40%, 80%), and after that the feed bounds
/// nothing. The answer bounds the sale however old it is and whether or not
/// the token's oracle is paused; a feed that gives no answer holds the sale
/// until the full band, and from it the time-weighted bound alone applies.
/// A claim's close is unchanged. Both token orders: a stock listed first in
/// its pool (the sale walks the price down) and one listed second (it walks
/// the price up).
contract AStoppedStationsSaleIsNeverMadeBelowTheFeedsLastAnswerLessItsDiscountTest is Test {
    uint256 internal constant WAD = 1e18;
    uint256 internal constant BPS = 10_000;
    uint32 internal constant WINDOW = 1800;
    uint256 internal constant MAX = 50e18;
    int24 internal constant SPACING = 10;
    /// The pools' price at tick 224,390 is about $180.03 a token; the feeds
    /// say $180.
    int24 internal constant AT_FEED = 224_390;
    uint256 internal constant FEED = 180e6;

    MockToken internal usdg;
    MockToken internal low;
    MockToken internal high;
    SkewedPool internal lowPool;
    SkewedPool internal highPool;
    MockFeed internal lowFeed;
    MockFeed internal highFeed;
    RobinhoodDriver internal driver;
    address internal station = makeAddr("station");
    address internal safe;
    int24 internal reach;

    function setUp() public {
        vm.warp(1_800_000_000);
        usdg = new MockToken(6);
        MockToken a = new MockToken(18);
        MockToken b = new MockToken(18);
        (MockToken first, MockToken second) = address(a) < address(b) ? (a, b) : (b, a);
        while (address(first) > address(usdg)) first = new MockToken(18);
        while (address(second) < address(usdg)) second = new MockToken(18);
        low = first;
        high = second;
        lowPool = new SkewedPool(address(low), address(usdg), -AT_FEED);
        highPool = new SkewedPool(address(usdg), address(high), AT_FEED);
        lowFeed = new MockFeed(180e8);
        highFeed = new MockFeed(180e8);
        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](2);
        listings[0] = RobinhoodDriver.Listing(
            address(low), IAggregatorV3(address(lowFeed)), IUniswapV3Pool(address(lowPool)), MAX
        );
        listings[1] = RobinhoodDriver.Listing(
            address(high), IAggregatorV3(address(highFeed)), IUniswapV3Pool(address(highPool)), MAX
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
                twapWindow: WINDOW
            }),
            listings
        );
        safe = driver.safeOf(station);
        vm.etch(safe, hex"00");
        reach = int24(int256(driver.SALE_SPACINGS())) * SPACING;
        low.mint(safe, 3 * MAX);
        high.mint(safe, 3 * MAX);
        usdg.mint(safe, 1000e6);
    }

    /// Both pools held `ticks` below the feed's price, for long enough that
    /// their time-weighted price is there too.
    function _hold(int24 ticks) internal {
        lowPool.set(-AT_FEED - ticks, -AT_FEED - ticks, false);
        highPool.set(AT_FEED + ticks, AT_FEED + ticks, false);
    }

    /// A stopped unwind's band in its `week` after the band became full.
    function _pastFull(uint256 week) internal pure returns (uint16) {
        return uint16(CloseBand.STOPPED | (BPS + week));
    }

    struct Sale {
        uint160 lowLimit;
        uint160 highLimit;
        uint256 lowIn;
        uint256 highIn;
        uint256 lowMinOut;
        uint256 highMinOut;
        bool lowSold;
        bool highSold;
        bool idle;
        bool blocked;
        uint256 calls;
    }

    function _close(uint16 band) internal view returns (Sale memory sale) {
        Call[] memory calls;
        (calls, sale.blocked) = driver.close(station, WAD, band);
        sale.calls = calls.length;
        for (uint256 i = 0; i < calls.length; i++) {
            (address to, bytes memory inner) = this.decodeExec(calls[i].data);
            if (to == address(usdg)) sale.idle = true;
            if (to != driver.router()) continue;
            ISwapRouter02.ExactInputSingleParams memory p = this.decodeSwap(inner);
            if (p.tokenIn == address(low)) {
                (sale.lowLimit, sale.lowIn, sale.lowMinOut, sale.lowSold) =
                    (p.sqrtPriceLimitX96, p.amountIn, p.amountOutMinimum, true);
            } else {
                (sale.highLimit, sale.highIn, sale.highMinOut, sale.highSold) =
                    (p.sqrtPriceLimitX96, p.amountIn, p.amountOutMinimum, true);
            }
        }
    }

    /// Dollar units one whole token fetches at a price limit: the pool's
    /// price is dollars per stock for the stock listed first, stock per
    /// dollar for the one listed second.
    function _lowDollars(uint160 limit) internal pure returns (uint256) {
        return Math.mulDiv(Math.mulDiv(WAD, limit, 1 << 96), limit, 1 << 96);
    }

    function _highDollars(uint160 limit) internal pure returns (uint256) {
        return Math.mulDiv(Math.mulDiv(WAD, 1 << 96, limit), 1 << 96, limit);
    }

    /// The sale stops at the feed less `discount`: never below it, and not
    /// short of it by more than the checks' rounding. At most `maxSale`, no
    /// minimum out, and reported not done.
    function _stopsAtTheFeedLess(Sale memory sale, uint256 discount, string memory at) internal pure {
        uint256 floor = FEED * (BPS - discount) / BPS;
        assertTrue(sale.lowSold && sale.highSold, string.concat(at, ": no sale"));
        assertGe(
            _lowDollars(sale.lowLimit) + 1, floor, string.concat(at, ": walking down, below the feed's bound")
        );
        assertGe(
            _highDollars(sale.highLimit) + 1, floor, string.concat(at, ": walking up, below the feed's bound")
        );
        assertLe(
            _lowDollars(sale.lowLimit),
            floor + 2,
            string.concat(at, ": walking down, short of the feed's bound")
        );
        assertLe(
            _highDollars(sale.highLimit),
            floor + 2,
            string.concat(at, ": walking up, short of the feed's bound")
        );
        assertEq(sale.lowIn, MAX, string.concat(at, ": other than one sale's maximum"));
        assertEq(sale.highIn, MAX, string.concat(at, ": other than one sale's maximum"));
        assertEq(sale.lowMinOut + sale.highMinOut, 0, string.concat(at, ": a minimum out"));
        assertTrue(sale.idle, string.concat(at, ": the idle dollars were held"));
        assertTrue(sale.blocked, string.concat(at, ": a close leaving stock unsold said it was done"));
    }

    /// The time-weighted bound for pools held `ticks` below the feed.
    function _twapBound(int24 ticks) internal view returns (uint160 lowTwap, uint160 highTwap) {
        lowTwap = UniswapPrice.sqrtRatioAtTick(-AT_FEED - ticks - reach);
        highTwap = UniswapPrice.sqrtRatioAtTick(AT_FEED + ticks + reach);
    }

    /// From the first week to the full band, a pool held off its price (as
    /// long as anyone likes, so its time-weighted price followed) is sold
    /// no lower than the feed less the band, at most 10%: the feed's bound
    /// is nearer than the time-weighted one, and the sale stops at it.
    function test_at_every_band_a_stopped_sale_stops_at_the_feed_less_the_band_at_most_ten_percent() public {
        uint16[8] memory bands = [uint16(100), 200, 400, 800, 1600, 3200, 6400, 10_000];
        int24[8] memory holds = [int24(50), 100, 300, 700, 900, 900, 900, 900];
        for (uint256 i = 0; i < bands.length; i++) {
            _hold(holds[i]);
            uint256 discount = bands[i] < 1000 ? bands[i] : 1000;
            _stopsAtTheFeedLess(
                _close(uint16(CloseBand.STOPPED | bands[i])), discount, vm.toString(uint256(bands[i]))
            );
        }
        // The full band given without the mark is the same sale.
        Sale memory unmarked = _close(10_000);
        Sale memory marked = _close(uint16(CloseBand.STOPPED | 10_000));
        assertEq(unmarked.lowLimit, marked.lowLimit);
        assertEq(unmarked.highLimit, marked.highLimit);
    }

    /// Held past the feed less the band: no sale at that band; the close
    /// says it isn't done and the idle dollars still go.
    function test_at_every_band_a_pool_held_past_the_feeds_bound_gets_no_sale() public {
        uint16[5] memory bands = [uint16(100), 400, 800, 3200, 10_000];
        int24[5] memory holds = [int24(150), 450, 900, 1150, 1150];
        for (uint256 i = 0; i < bands.length; i++) {
            _hold(holds[i]);
            Sale memory sale = _close(uint16(CloseBand.STOPPED | bands[i]));
            assertFalse(sale.lowSold || sale.highSold, "sold past the feed's bound");
            assertTrue(sale.idle, "the idle dollars were held with the sales");
            assertTrue(sale.blocked, "a close leaving stock unsold said it was done");
        }
    }

    /// From the full band the discount doubles each week (10%, 20%, 40%,
    /// 80%), and from the fourth week after it the feed bounds nothing: the
    /// time-weighted bound alone applies, however far the pool is.
    function test_past_the_full_band_the_discount_doubles_each_week_and_ends_after_eighty_percent() public {
        uint256[4] memory discounts = [uint256(1000), 2000, 4000, 8000];
        int24[4] memory holds = [int24(900), 2000, 4900, 15_900];
        // Past each week's bound: about 11.3%, 22.1%, 41.1%, 80.4% under.
        int24[4] memory past = [int24(1200), 2500, 5300, 16_300];
        for (uint256 week = 0; week < 4; week++) {
            _hold(holds[week]);
            _stopsAtTheFeedLess(_close(_pastFull(week)), discounts[week], vm.toString(week));
            _hold(past[week]);
            Sale memory held = _close(_pastFull(week));
            assertFalse(held.lowSold || held.highSold, "sold past the week's feed bound");
        }
        uint256[4] memory later = [uint256(4), 5, 52, 0x7FFF - BPS];
        for (uint256 i = 0; i < later.length; i++) {
            _hold(20_000);
            (uint160 lowTwap, uint160 highTwap) = _twapBound(20_000);
            Sale memory sale = _close(_pastFull(later[i]));
            assertEq(sale.lowLimit, lowTwap, "walking down, not the time-weighted bound alone");
            assertEq(sale.highLimit, highTwap, "walking up, not the time-weighted bound alone");
            assertEq(sale.lowIn + sale.highIn, 2 * MAX);
        }
    }

    /// A feed older than the most it may be for a valuation, even by a
    /// year, or whose token's oracle is paused, still bounds the sale at
    /// its last answer, at a band inside the full one and at the full band.
    function test_a_stale_or_paused_feed_still_bounds_the_sale_at_its_last_answer() public {
        uint16[2] memory bands = [uint16(CloseBand.STOPPED | 400), uint16(CloseBand.STOPPED | 10_000)];
        int24[2] memory holds = [int24(300), 900];
        uint256[2] memory discounts = [uint256(400), 1000];
        for (uint256 b = 0; b < bands.length; b++) {
            for (uint256 i = 0; i < 4; i++) {
                uint256 snapshot = vm.snapshotState();
                _hold(holds[b]);
                if (i == 1) {
                    lowFeed.set(180e8, 4 days + 1);
                    highFeed.set(180e8, 4 days + 1);
                } else if (i == 2) {
                    lowFeed.set(180e8, 365 days);
                    highFeed.set(180e8, 365 days);
                } else if (i == 3) {
                    vm.mockCall(address(low), abi.encodeWithSignature("oraclePaused()"), abi.encode(true));
                    vm.mockCall(address(high), abi.encodeWithSignature("oraclePaused()"), abi.encode(true));
                }
                _stopsAtTheFeedLess(_close(bands[b]), discounts[b], vm.toString(i));
                // And held past it, no sale.
                _hold(holds[b] + 250);
                Sale memory held = _close(bands[b]);
                assertFalse(held.lowSold || held.highSold, "sold past a stale or paused feed's bound");
                vm.revertToState(snapshot);
                vm.clearMockedCalls();
            }
        }
    }

    /// A feed that gives no answer (zero, negative, or a read that fails)
    /// holds the sale until the full band: nothing is sold, the close says
    /// it isn't done, and the idle dollars still go. From the full band the
    /// time-weighted bound alone applies, and the sale is made.
    function test_a_feed_with_no_answer_holds_the_sale_until_the_full_band_then_the_time_weighted_bound_applies(
    ) public {
        _hold(2000);
        (uint160 lowTwap, uint160 highTwap) = _twapBound(2000);
        for (uint256 i = 0; i < 3; i++) {
            uint256 snapshot = vm.snapshotState();
            if (i == 2) {
                vm.mockCallRevert(address(lowFeed), abi.encodeWithSignature("latestRoundData()"), "");
                vm.mockCallRevert(address(highFeed), abi.encodeWithSignature("latestRoundData()"), "");
            } else {
                int256 answer = i == 0 ? int256(0) : -180e8;
                lowFeed.set(answer, 0);
                highFeed.set(answer, 0);
            }
            uint16[3] memory inside = [uint16(100), 1600, 6400];
            for (uint256 b = 0; b < inside.length; b++) {
                Sale memory held = _close(uint16(CloseBand.STOPPED | inside[b]));
                assertFalse(held.lowSold || held.highSold, "sold with no answer before the full band");
                assertTrue(held.idle && held.blocked);
            }
            Sale memory sale = _close(uint16(CloseBand.STOPPED | 10_000));
            assertEq(sale.lowLimit, lowTwap, "walking down, not the time-weighted bound");
            assertEq(sale.highLimit, highTwap, "walking up, not the time-weighted bound");
            assertTrue(sale.idle && sale.blocked);
            vm.revertToState(snapshot);
            vm.clearMockedCalls();
        }
    }

    /// A claim's close (the exit band, unmarked) is as it was: the whole
    /// fraction, with no price limit and a minimum out at the feed less the
    /// band, done in one close; blocked, with no calls, when the market is
    /// further from the feed than the band or the feed can't be trusted.
    function test_a_claims_close_is_unchanged() public {
        Sale memory sale = _close(100);
        assertFalse(sale.blocked);
        assertEq(sale.lowIn, 3 * MAX, "a claim's close was cut to one sale's maximum");
        assertEq(sale.highIn, 3 * MAX, "a claim's close was cut to one sale's maximum");
        assertEq(sale.lowLimit, 0, "a claim's close has a price limit");
        assertEq(sale.highLimit, 0, "a claim's close has a price limit");
        uint256 minOut = Math.mulDiv(3 * MAX, 180e8, 1e20) * 9900 / BPS;
        assertEq(sale.lowMinOut, minOut, "a claim's close's minimum out moved");
        assertEq(sale.highMinOut, minOut, "a claim's close's minimum out moved");
        // Half of it: half of each holding.
        Sale memory half = _close_(WAD / 2, 100);
        assertEq(half.lowIn, 3 * MAX / 2);
        // A market 2% under the feed, or a feed a second too old: blocked.
        _hold(200);
        Sale memory outside = _close(100);
        assertTrue(outside.blocked);
        assertEq(outside.calls, 0);
        _hold(0);
        lowFeed.set(180e8, 4 days + 1);
        Sale memory stale = _close(100);
        assertTrue(stale.blocked);
        assertEq(stale.calls, 0);
    }

    function _close_(uint256 fraction, uint16 band) internal view returns (Sale memory sale) {
        Call[] memory calls;
        (calls, sale.blocked) = driver.close(station, fraction, band);
        for (uint256 i = 0; i < calls.length; i++) {
            (address to, bytes memory inner) = this.decodeExec(calls[i].data);
            if (to != driver.router()) continue;
            ISwapRouter02.ExactInputSingleParams memory p = this.decodeSwap(inner);
            if (p.tokenIn == address(low)) sale.lowIn = p.amountIn;
        }
    }

    function decodeExec(bytes calldata data) external pure returns (address to, bytes memory inner) {
        (to,, inner,,,,,,,) = abi.decode(
            data[4:], (address, uint256, bytes, uint8, uint256, uint256, uint256, address, address, bytes)
        );
    }

    function decodeSwap(bytes calldata data)
        external
        pure
        returns (ISwapRouter02.ExactInputSingleParams memory p)
    {
        p = abi.decode(data[4:], (ISwapRouter02.ExactInputSingleParams));
    }
}
