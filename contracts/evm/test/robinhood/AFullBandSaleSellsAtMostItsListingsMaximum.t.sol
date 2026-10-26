// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

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
import {StationCloses} from "../../src/station/StationCloses.sol";
import {
    MockFeed, MockPool, MockSafeFactory, MockToken
} from "./AStockCloseRoundsByUnderAUnitPerFigure.t.sol";
import {Test} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// At the full band a stopped station's unwind sells each token at any
/// price, through calls the station gives a fixed allowance of gas each. So
/// the Robinhood driver sells at most its listing's maximum of a token per
/// close, sized so that one sale fits that allowance with room to spare,
/// and no further than a fixed number of its pool's tick spacings from the
/// pool's price, so the sale's path is bounded too; a close that sells
/// reports itself blocked beside its calls, and the next unwind sells on,
/// until a close finds nothing left to sell. Inside the band a close sells its whole fraction,
/// as before. And it lists few enough tokens that its worst full unwind,
/// every call spending its whole allowance, fits EIP-7825's per-transaction
/// gas cap.
contract AFullBandSaleSellsAtMostItsListingsMaximumTest is Test {
    uint256 internal constant WAD = 1e18;
    uint32 internal constant WINDOW = 1800;
    uint256 internal constant NVDA_MAX = 50e18;
    uint256 internal constant AAPL_MAX = 40e18;
    /// EIP-7825's per-transaction gas cap, which Base applies.
    uint256 internal constant TX_GAS_CAP = 16_777_216;
    /// What the driver's reads in a full unwind spend at the most for its
    /// most listings: under 300,000 a listing on a Base fork (293,472 measured).
    uint256 internal constant DRIVER_READS = 700_000;
    /// The station's and the vault's own work around the driver's part.
    uint256 internal constant AROUND = 1_000_000;

    MockToken internal usdg;
    MockToken internal nvda;
    MockToken internal aapl;
    RobinhoodDriver internal driver;
    address internal station = makeAddr("station");
    address internal safe;
    MockFeed internal nvdaFeed;
    MockFeed internal aaplFeed;
    MockPool internal nvdaPool;
    MockPool internal aaplPool;

    function setUp() public {
        usdg = new MockToken(6);
        nvda = new MockToken(18);
        aapl = new MockToken(18);
        nvdaPool = _pool(address(nvda), 224_390);
        aaplPool = _pool(address(aapl), 221_100);
        nvdaFeed = new MockFeed(_feedFromTick(nvdaPool.tick(), address(nvda)));
        aaplFeed = new MockFeed(_feedFromTick(aaplPool.tick(), address(aapl)));
        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](2);
        listings[0] = RobinhoodDriver.Listing(
            address(nvda), IAggregatorV3(address(nvdaFeed)), IUniswapV3Pool(address(nvdaPool)), NVDA_MAX
        );
        listings[1] = RobinhoodDriver.Listing(
            address(aapl), IAggregatorV3(address(aaplFeed)), IUniswapV3Pool(address(aaplPool)), AAPL_MAX
        );
        driver = new RobinhoodDriver(_venue(), listings);
        safe = driver.safeOf(station);
        vm.etch(safe, hex"00");
    }

    function _venue() internal returns (RobinhoodDriver.Venue memory) {
        return RobinhoodDriver.Venue({
            dollar: IERC20(address(usdg)),
            router: makeAddr("router"),
            safeSingleton: makeAddr("singleton"),
            safeFactory: ISafeProxyFactory(address(new MockSafeFactory())),
            rolesMastercopy: makeAddr("roles"),
            moduleFactory: IModuleProxyFactory(makeAddr("moduleFactory")),
            maxFeedAge: 4 days,
            twapWindow: WINDOW
        });
    }

    function _pool(address token, int24 tickIfDollarFirst) internal returns (MockPool) {
        return address(usdg) < token
            ? new MockPool(address(usdg), token, tickIfDollarFirst)
            : new MockPool(token, address(usdg), -tickIfDollarFirst);
    }

    function _feedFromTick(int24 tick, address token) internal view returns (int256) {
        return int256(UniswapPrice.quoteAtTick(tick, 1e18, token, address(usdg)) * 100);
    }

    /// What each token's sale in `calls` sells, and the idle dollars sent.
    function _sold(Call[] memory calls)
        internal
        view
        returns (uint256 nvdaSold, uint256 aaplSold, uint256 idle)
    {
        for (uint256 i = 0; i < calls.length; i++) {
            (address to, bytes memory inner) = this.decodeExec(calls[i].data);
            if (to == driver.router()) {
                ISwapRouter02.ExactInputSingleParams memory p = this.decodeSwap(inner);
                if (p.tokenIn == address(nvda)) nvdaSold += p.amountIn;
                else aaplSold += p.amountIn;
            } else if (to == address(usdg)) {
                (, idle) = this.decodeTransfer(inner);
            }
        }
    }

    function test_at_the_full_band_a_close_sells_at_most_each_listings_maximum_and_is_not_done() public {
        nvda.mint(safe, 3 * NVDA_MAX + 1);
        aapl.mint(safe, AAPL_MAX / 2);
        usdg.mint(safe, 1000e6);
        (Call[] memory calls, bool blocked) = driver.close(station, WAD, 10_000);
        assertTrue(blocked, "a close that leaves some of a holding unsold reported itself done");
        assertEq(calls.length, 5, "an approval and a sale per token, and the idle dollars");
        (uint256 nvdaSold, uint256 aaplSold, uint256 idle) = _sold(calls);
        assertEq(nvdaSold, NVDA_MAX, "more than the listing's maximum was sold");
        assertEq(aaplSold, AAPL_MAX / 2, "a holding under its maximum was not sold whole");
        assertEq(idle, 1000e6, "the idle dollars were not sent beside the sales");
    }

    function test_at_the_full_band_holdings_within_their_maximum_are_offered_whole_and_done_once_sold()
        public
    {
        nvda.mint(safe, NVDA_MAX);
        aapl.mint(safe, AAPL_MAX);
        (Call[] memory calls, bool blocked) = driver.close(station, WAD, 10_000);
        // What the sale's price limit leaves is known only once it's made.
        assertTrue(blocked, "a close that sells said it was done");
        (uint256 nvdaSold, uint256 aaplSold,) = _sold(calls);
        assertEq(nvdaSold, NVDA_MAX);
        assertEq(aaplSold, AAPL_MAX);
        // Sold: the next close has nothing to sell and is done.
        nvda.burn(safe, NVDA_MAX);
        aapl.burn(safe, AAPL_MAX);
        (calls, blocked) = driver.close(station, WAD, 10_000);
        assertFalse(blocked, "a close with nothing left to sell said it wasn't done");
        assertEq(calls.length, 0);
    }

    /// Each full-band sale stops `SALE_SPACINGS` of its pool's tick spacings
    /// from the pool's price, in the sale's direction: down for a token the
    /// pool lists first, up for one it lists second. Inside the band a sale
    /// has no price limit (its minimum out bounds it).
    function test_a_full_band_sale_stops_a_fixed_number_of_tick_spacings_from_the_pools_price() public {
        nvda.mint(safe, NVDA_MAX);
        aapl.mint(safe, AAPL_MAX);
        (Call[] memory calls,) = driver.close(station, WAD, 10_000);
        uint256 checked;
        for (uint256 i = 0; i < calls.length; i++) {
            (address to, bytes memory inner) = this.decodeExec(calls[i].data);
            if (to != driver.router()) continue;
            ISwapRouter02.ExactInputSingleParams memory p = this.decodeSwap(inner);
            MockPool pool = p.tokenIn == address(nvda) ? nvdaPool : aaplPool;
            int24 distance = int24(int256(driver.SALE_SPACINGS())) * 10;
            bool down = p.tokenIn < address(usdg);
            int24 limitTick = down ? pool.tick() - distance : pool.tick() + distance;
            assertEq(p.sqrtPriceLimitX96, UniswapPrice.sqrtRatioAtTick(limitTick), "the sale's price limit");
            checked++;
        }
        assertEq(checked, 2);
        (calls,) = driver.close(station, WAD, 500);
        for (uint256 i = 0; i < calls.length; i++) {
            (address to, bytes memory inner) = this.decodeExec(calls[i].data);
            if (to == driver.router()) assertEq(this.decodeSwap(inner).sqrtPriceLimitX96, 0);
        }
    }

    function test_inside_the_band_a_close_sells_its_whole_fraction_whatever_the_maximum() public {
        nvda.mint(safe, 10 * NVDA_MAX);
        (Call[] memory calls, bool blocked) = driver.close(station, WAD / 2, 500);
        assertFalse(blocked);
        (uint256 nvdaSold,,) = _sold(calls);
        assertEq(nvdaSold, 5 * NVDA_MAX);
    }

    /// What is left unsold is valued as before: the same tokens at the same
    /// prices, never more.
    function test_what_a_bounded_sale_leaves_is_valued_as_before() public {
        nvda.mint(safe, 3 * NVDA_MAX);
        (uint256 markBefore, uint256 exitBefore) = driver.valueOf(station);
        (Call[] memory calls,) = driver.close(station, WAD, 10_000);
        (uint256 nvdaSold,,) = _sold(calls);
        nvda.burn(safe, nvdaSold);
        (uint256 markAfter, uint256 exitAfter) = driver.valueOf(station);
        assertLe(markAfter, markBefore * 2 / 3 + 1);
        assertLe(exitAfter, exitBefore * 2 / 3 + 1);
        assertGt(exitAfter, 0, "what is left is no longer counted");
    }

    /// Each listing's maximum is the driver's own, read by token; the
    /// listed stocks read as before.
    function test_each_listings_maximum_is_kept_by_token() public view {
        assertEq(driver.maxSale(address(nvda)), NVDA_MAX);
        assertEq(driver.maxSale(address(aapl)), AAPL_MAX);
        assertEq(driver.stocks().length, 2);
    }

    function test_a_listing_without_a_maximum_sale_is_refused() public {
        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](1);
        listings[0] = RobinhoodDriver.Listing(
            address(nvda), IAggregatorV3(address(nvdaFeed)), IUniswapV3Pool(address(nvdaPool)), 0
        );
        RobinhoodDriver.Venue memory venue = _venue();
        vm.expectRevert(RobinhoodDriver.NoMaxSale.selector);
        new RobinhoodDriver(venue, listings);
    }

    /// A window of zero would leave no pool with a time-weighted price, so
    /// no full-band sale and no exit value, for good: it is refused.
    function test_a_time_weighted_window_of_zero_is_refused() public {
        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](1);
        listings[0] = RobinhoodDriver.Listing(
            address(nvda), IAggregatorV3(address(nvdaFeed)), IUniswapV3Pool(address(nvdaPool)), NVDA_MAX
        );
        RobinhoodDriver.Venue memory venue = _venue();
        venue.twapWindow = 0;
        vm.expectRevert(RobinhoodDriver.NoTwapWindow.selector);
        new RobinhoodDriver(venue, listings);
    }

    /// A feed age of zero would trust no feed after its own block, so every
    /// valuation would be void: it is refused.
    function test_a_feed_age_of_zero_is_refused() public {
        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](1);
        listings[0] = RobinhoodDriver.Listing(
            address(nvda), IAggregatorV3(address(nvdaFeed)), IUniswapV3Pool(address(nvdaPool)), NVDA_MAX
        );
        RobinhoodDriver.Venue memory venue = _venue();
        venue.maxFeedAge = 0;
        vm.expectRevert(RobinhoodDriver.NoMaxFeedAge.selector);
        new RobinhoodDriver(venue, listings);
    }

    /// The worst full unwind through the driver: its reads (at most
    /// `DRIVER_READS`, measured on a fork), then an approval and a sale per
    /// listed token and the idle dollars, each spending its whole
    /// allowance, the last with its whole floor left, inside the part's
    /// call (which keeps back 1/64), with room for the station's and the
    /// vault's work around it, fits the cap. One more token would not.
    function test_the_driver_lists_few_enough_tokens_for_its_worst_full_unwind_to_fit_the_cap() public view {
        assertLe(
            _worst(driver.MAX_LISTINGS()) + AROUND, TX_GAS_CAP, "the worst full unwind does not fit the cap"
        );
        assertGt(_worst(driver.MAX_LISTINGS() + 1) + AROUND, TX_GAS_CAP, "the driver could list more");
    }

    function _worst(uint256 listings) internal pure returns (uint256) {
        uint256 calls = 2 * listings + 1;
        uint256 part = DRIVER_READS + (calls - 1) * StationCloses.ATTEMPT_GAS + StationCloses.ATTEMPT_FLOOR;
        return part + part / 63;
    }

    // Decoders (external so calldata slices can be used) -------------------

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

    function decodeTransfer(bytes calldata data) external pure returns (address to, uint256 amount) {
        (to, amount) = abi.decode(data[4:], (address, uint256));
    }
}
