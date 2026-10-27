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
import {Call, IDriver} from "../../src/interfaces/IDriver.sol";
import {StationCloses} from "../../src/station/StationCloses.sol";
import {IUniswapV3PoolForSale} from "./AFullBandSaleOnBaseFitsItsAllowance.t.sol";
import {StationDouble} from "./StationDouble.sol";
import {Test, console2} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

interface IPoolPosition {
    function burn(int24 tickLower, int24 tickUpper, uint128 amount) external returns (uint256, uint256);
    function collect(
        address recipient,
        int24 tickLower,
        int24 tickUpper,
        uint128 amount0Requested,
        uint128 amount1Requested
    ) external returns (uint128, uint128);
}

/// A stopped station's full-band stock sale on Robinhood Chain's real
/// NVDA/USDG and AAPL/USDG pools, with their real feeds, Safe and Roles
/// (the mainnet listings and maximums), against a party who pushes a pool
/// its spacings past the time-weighted price, holds it there a window,
/// and then bids at the floor for what the station sells: each window the
/// time-weighted bound follows the held pool, but no unit is sold below the
/// feed's latest answer less `FULL_BAND_FEED_BPS` (less the pool's fee),
/// fresh or stale. Four weeks past the full band the feed bounds nothing
/// and the time-weighted bound alone applies. Runs only with
/// `ROBINHOOD_FORK_URL` (at the latest block, on a weekday when the stock
/// feeds are fresh). Nothing is sent to the chain.
abstract contract RobinhoodPoolsForkTest is Test {
    address internal constant USDG = 0x5fc5360D0400a0Fd4f2af552ADD042D716F1d168;
    address internal constant ROUTER = 0xCaf681a66D020601342297493863E78C959E5cb2;
    address internal constant NVDA = 0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC;
    address internal constant NVDA_POOL = 0xd4EB21209C4D6093f80B5b84f5C45cc093EA14a3;
    address internal constant NVDA_FEED = 0x379EC4f7C378F34a1B47E4F3cbeBCbAC3E8E9F15;
    address internal constant AAPL = 0xaF3D76f1834A1d425780943C99Ea8A608f8a93f9;
    address internal constant AAPL_POOL = 0xAae0d815EE56e4092a5E5C2911E676Fea50B2d6D;
    address internal constant AAPL_FEED = 0x6B22A786bAa607d76728168703a39Ea9C99f2cD0;
    address internal constant SAFE_SINGLETON = 0x29fcB43b46531BcA003ddC8FCB67FFE91900C762;
    address internal constant SAFE_FACTORY = 0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67;
    address internal constant ROLES_MASTERCOPY = 0x9646fDAD06d3e24444381f44362a3B0eB343D337;
    address internal constant MODULE_FACTORY = 0x000000000000aDdB49795b0f9bA5BC298cDda236;
    uint256 internal constant MAX_FEED_AGE = 4 days;
    uint32 internal constant WINDOW = 30 minutes;
    int24 internal constant SPACING = 10;

    RobinhoodDriver internal driver;
    StationDouble internal station;
    address internal safe;
    bool internal forked;

    /// The listings' `maxSale`: mainnet's unless a test says otherwise.
    function _maxSale(address stock) internal pure virtual returns (uint256) {
        return stock == NVDA ? 1000 ether : 100 ether;
    }

    function setUp() public {
        string memory url = vm.envOr("ROBINHOOD_FORK_URL", string(""));
        if (bytes(url).length == 0) return;
        vm.createSelectFork(url);
        forked = true;
        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](2);
        listings[0] =
            RobinhoodDriver.Listing(NVDA, IAggregatorV3(NVDA_FEED), IUniswapV3Pool(NVDA_POOL), _maxSale(NVDA));
        listings[1] =
            RobinhoodDriver.Listing(AAPL, IAggregatorV3(AAPL_FEED), IUniswapV3Pool(AAPL_POOL), _maxSale(AAPL));
        driver = new RobinhoodDriver(
            RobinhoodDriver.Venue({
                dollar: IERC20(USDG),
                router: ROUTER,
                safeSingleton: SAFE_SINGLETON,
                safeFactory: ISafeProxyFactory(SAFE_FACTORY),
                rolesMastercopy: ROLES_MASTERCOPY,
                moduleFactory: IModuleProxyFactory(MODULE_FACTORY),
                maxFeedAge: MAX_FEED_AGE,
                twapWindow: WINDOW
            }),
            listings
        );
        station = new StationDouble(USDG, 100);
        station.approveKey(IDriver(address(driver)), makeAddr("executor"));
        safe = driver.safeOf(address(station));
        deal(USDG, address(this), 1_000_000_000e6);
        deal(NVDA, address(this), 1_000_000 ether);
        deal(AAPL, address(this), 1_000_000 ether);
        IERC20(USDG).approve(ROUTER, type(uint256).max);
        IERC20(NVDA).approve(ROUTER, type(uint256).max);
        IERC20(AAPL).approve(ROUTER, type(uint256).max);
    }

    modifier onFork() {
        if (!forked) {
            vm.skip(true);
            return;
        }
        _;
    }

    function uniswapV3MintCallback(uint256 amount0, uint256 amount1, bytes calldata) external {
        if (amount0 != 0) IERC20(IUniswapV3Pool(msg.sender).token0()).transfer(msg.sender, amount0);
        if (amount1 != 0) IERC20(IUniswapV3Pool(msg.sender).token1()).transfer(msg.sender, amount1);
    }

    function _swap(
        address tokenIn,
        address tokenOut,
        uint256 amount,
        uint160 limit
    ) internal returns (uint256) {
        return ISwapRouter02(ROUTER).exactInputSingle(
            ISwapRouter02.ExactInputSingleParams(tokenIn, tokenOut, 500, address(this), amount, 0, limit)
        );
    }

    /// The feed's price of one whole token (USDG units, 6 dp), and whether
    /// it is fresh enough for the driver to trust.
    function _feed(address feed) internal view returns (uint256 price, bool fresh) {
        (, int256 answer,, uint256 updatedAt,) = IAggregatorV3(feed).latestRoundData();
        price = uint256(answer) / 100;
        fresh = block.timestamp - updatedAt <= MAX_FEED_AGE;
    }

    /// The feed's price less the discount, less the pool's fee: the least a
    /// token of a full-band sale may fetch.
    function _floor(uint256 feedPrice) internal view returns (uint256) {
        return feedPrice * (10_000 - driver.FULL_BAND_FEED_BPS()) / 10_000 * 9995 / 10_000;
    }

    /// The whole holding's sale at a stopped unwind's `band`, call by call
    /// as `closeAll` makes it, repeated within the block until nothing more
    /// sells (a contract may call the unwind again within one transaction).
    /// What it sold and what it fetched, and the most one close sold.
    function _sellAll(address stock) internal returns (uint256 sold, uint256 fetched) {
        (sold, fetched,) = _sellAll(stock, 10_000);
    }

    function _sellAll(
        address stock,
        uint16 band
    ) internal returns (uint256 sold, uint256 fetched, uint256 mostInOneClose) {
        uint256 held = IERC20(stock).balanceOf(safe);
        uint256 home = IERC20(USDG).balanceOf(address(station));
        for (uint256 round = 0; round < 40; round++) {
            uint256 once = _closeOnce(stock, band);
            if (once > mostInOneClose) mostInOneClose = once;
            if (once == 0) break;
        }
        sold = held - IERC20(stock).balanceOf(safe);
        fetched = IERC20(USDG).balanceOf(address(station)) - home;
    }

    /// One close at `band`, call by call as `closeAll` makes it: how much of
    /// `stock` it sold.
    function _closeOnce(address stock, uint16 band) internal returns (uint256) {
        (Call[] memory list,) = driver.close(address(station), 1e18, band);
        uint256 left = IERC20(stock).balanceOf(safe);
        for (uint256 i = 0; i < list.length; i++) {
            (bool ok,) = station.attempt(IDriver(address(driver)), list[i], StationCloses.ATTEMPT_GAS);
            assertTrue(ok, "a call failed within its allowance");
        }
        return left - IERC20(stock).balanceOf(safe);
    }

    /// Sell `stock` into `pool` until its tick is `past` ticks above the
    /// time-weighted tick (the stock is token1: selling it makes it
    /// cheaper), and hold it there for a window.
    function _pushAndHold(address stock, address pool, int24 past) internal returns (int24 meanAfter) {
        int24 target = UniswapPrice.meanTick(IUniswapV3Pool(pool), WINDOW, false) + past;
        _swap(stock, USDG, 1_000_000 ether, UniswapPrice.sqrtRatioAtTick(target));
        (, int24 tick,,,,,) = IUniswapV3Pool(pool).slot0();
        assertGe(tick, target - 1, "the push did not reach its target");
        vm.warp(vm.getBlockTimestamp() + WINDOW);
        vm.roll(vm.getBlockNumber() + 1);
        meanAfter = UniswapPrice.meanTick(IUniswapV3Pool(pool), WINDOW, false);
    }

    /// The pusher's dollars as liquidity from the pool's tick to the
    /// time-weighted bound, so the station's sale fills there, into their
    /// hands; then taken out again.
    function _sellIntoABid(
        address stock,
        address pool,
        int24 meanAfter
    ) internal returns (uint256 sold, uint256 fetched) {
        (sold, fetched) = _sellIntoABid(stock, pool, meanAfter, 10_000);
    }

    function _sellIntoABid(
        address stock,
        address pool,
        int24 meanAfter,
        uint16 band
    ) internal returns (uint256 sold, uint256 fetched) {
        _bid(pool, meanAfter);
        (sold, fetched,) = _sellAll(stock, band);
        _unbid(pool);
    }

    /// The pusher's bid, kept here (not on the stack) between the push and
    /// taking it out.
    int24 internal bidLower;
    int24 internal bidUpper;

    /// The pusher's bid, from just past the pool's tick to just past the
    /// time-weighted bound.
    function _bid(address pool, int24 meanAfter) internal {
        (, int24 tick,,,,,) = IUniswapV3Pool(pool).slot0();
        int24 lower = (tick + 1) / SPACING * SPACING;
        if (lower < tick + 1) lower += SPACING;
        _bid(
            pool,
            lower,
            (meanAfter + int24(int256(driver.SALE_SPACINGS())) * SPACING) / SPACING * SPACING + SPACING
        );
    }

    function _bid(address pool, int24 lower, int24 upper) internal {
        (bidLower, bidUpper) = (lower, upper);
        IUniswapV3PoolForSale(pool).mint(address(this), lower, upper, 2e21, "");
    }

    function _unbid(address pool) internal {
        IPoolPosition(pool).burn(bidLower, bidUpper, 2e21);
        IPoolPosition(pool).collect(address(this), bidLower, bidUpper, type(uint128).max, type(uint128).max);
    }
}

contract AStockPoolHeldOffItsPriceCannotSellBelowTheFeedLessItsDiscountTest is RobinhoodPoolsForkTest {
    /// Window after window the pool is pushed a spacing short of the
    /// time-weighted bound and held; each window the station sells one
    /// `maxSale` into the pusher's bid. The time-weighted bound falls by
    /// the spacings each window (about 2.5%), but no sale is ever made
    /// below the feed less the discount.
    function _ratchet(
        address stock,
        address pool,
        address feed,
        uint256 windows
    ) internal returns (uint256 worstBps, uint256 soldBelowTheOldBound) {
        vm.warp(vm.getBlockTimestamp() + WINDOW);
        (uint256 feedPrice, bool fresh) = _feed(feed);
        assertTrue(fresh, "the feed is not fresh at this block");
        uint256 floor = _floor(feedPrice);
        int24 past = int24(int256(driver.SALE_SPACINGS())) * SPACING - 1;
        for (uint256 w = 1; w <= windows; w++) {
            deal(stock, safe, _maxSale(stock));
            int24 meanAfter = _pushAndHold(stock, pool, past);
            (uint256 sold, uint256 fetched) = _sellIntoABid(stock, pool, meanAfter);
            if (sold == 0) {
                console2.log("window: nothing sold", w);
                continue;
            }
            uint256 price = fetched * 1 ether / sold;
            uint256 bps = price >= feedPrice ? 0 : (feedPrice - price) * 10_000 / feedPrice;
            console2.log("window / sold (1e18) / price (USDG, 6 dp)", w, sold, price);
            console2.log("window / under the feed (bps)", w, bps);
            assertGe(price, floor, "sold below the feed less its discount");
            if (bps > worstBps) worstBps = bps;
            if (bps > driver.FULL_BAND_FEED_BPS()) soldBelowTheOldBound += sold;
        }
        // Held long enough that the time-weighted bound alone would have
        // sold below the feed's bound: the ratchet stopped there.
        assertLe(worstBps, driver.FULL_BAND_FEED_BPS() + 5, "the ratchet went past the feed's bound");
    }

    function test_the_real_nvda_pool_held_off_its_price_for_windows_cannot_sell_below_the_feed_less_the_discount(
    ) public onFork {
        _ratchet(NVDA, NVDA_POOL, NVDA_FEED, 6);
    }

    function test_the_real_aapl_pool_held_off_its_price_for_windows_cannot_sell_below_the_feed_less_the_discount(
    ) public onFork {
        _ratchet(AAPL, AAPL_POOL, AAPL_FEED, 6);
    }

    /// One push past the feed's bound, held a window: whether the feed is
    /// fresh or older than `maxFeedAge`, no NVDA is sold (a stale feed
    /// keeps its last answer); four weeks past the full band the feed
    /// bounds nothing, and the sale is made below the feed's bound but
    /// never below the time-weighted price less the spacings and the fee.
    function _pushPastTheFeedsBound() internal returns (int24 meanAfter, uint256 feedPrice) {
        (feedPrice,) = _feed(NVDA_FEED);
        // About 12% under the price: past the feed's 10%.
        meanAfter = _pushAndHold(NVDA, NVDA_POOL, 1250);
        deal(NVDA, safe, _maxSale(NVDA));
    }

    function test_on_the_real_nvda_pool_a_fresh_feed_holds_a_sale_past_its_bound() public onFork {
        vm.warp(vm.getBlockTimestamp() + WINDOW);
        (, bool fresh) = _feed(NVDA_FEED);
        assertTrue(fresh, "the feed is not fresh at this block");
        (int24 meanAfter,) = _pushPastTheFeedsBound();
        (uint256 sold,) = _sellIntoABid(NVDA, NVDA_POOL, meanAfter);
        assertEq(sold, 0, "sold past the feed's bound");
        assertEq(IERC20(NVDA).balanceOf(safe), _maxSale(NVDA), "the holding moved");
    }

    function test_on_the_real_nvda_pool_a_stale_feed_still_holds_a_sale_past_its_last_answers_bound()
        public
        onFork
    {
        vm.warp(vm.getBlockTimestamp() + MAX_FEED_AGE + 1);
        (, bool fresh) = _feed(NVDA_FEED);
        assertFalse(fresh, "the feed is still fresh");
        (int24 meanAfter,) = _pushPastTheFeedsBound();
        (uint256 sold,) = _sellIntoABid(NVDA, NVDA_POOL, meanAfter);
        assertEq(sold, 0, "sold past a stale feed's bound");
        assertEq(IERC20(NVDA).balanceOf(safe), _maxSale(NVDA), "the holding moved");
    }

    function test_on_the_real_nvda_pool_four_weeks_past_the_full_band_the_time_weighted_bound_alone_applies()
        public
        onFork
    {
        vm.warp(vm.getBlockTimestamp() + MAX_FEED_AGE + 1);
        (int24 meanAfter, uint256 feedPrice) = _pushPastTheFeedsBound();
        (uint256 sold, uint256 fetched) =
            _sellIntoABid(NVDA, NVDA_POOL, meanAfter, uint16(CloseBand.STOPPED | (10_000 + 4)));
        assertEq(sold, _maxSale(NVDA), "the sale was not made");
        uint256 price = fetched * 1 ether / sold;
        uint256 twapFloor = UniswapPrice.quoteAtTick(
            meanAfter + int24(int256(driver.SALE_SPACINGS())) * SPACING, 1 ether, NVDA, USDG
        ) * 9995 / 10_000;
        console2.log(
            "price (USDG, 6 dp) / the feed's bound / the time-weighted bound",
            price,
            _floor(feedPrice),
            twapFloor
        );
        assertLt(price, _floor(feedPrice), "the feed still bounded the sale");
        assertGe(price, twapFloor, "sold below the time-weighted bound");
    }
}

/// A full-band sale is bounded by its price limit, not by a minimum out, so
/// a sale the limit stops part way (a holding larger than the pool takes
/// within it) still sells what it can at no less than the bound, and the
/// next unwind sells on. Here a listing whose `maxSale` is larger than the
/// real NVDA pool takes before the feed's bound.
contract AFullBandSaleStoppedPartWayByItsLimitStillSellsTest is RobinhoodPoolsForkTest {
    function _maxSale(address stock) internal pure override returns (uint256) {
        return stock == NVDA ? 50_000 ether : 100 ether;
    }

    function test_a_full_band_sale_its_limit_stops_part_way_still_sells() public onFork {
        vm.warp(vm.getBlockTimestamp() + WINDOW);
        (uint256 feedPrice, bool fresh) = _feed(NVDA_FEED);
        assertTrue(fresh, "the feed is not fresh at this block");
        uint256 holding = 50_000 ether;
        deal(NVDA, safe, holding);
        uint256 home = IERC20(USDG).balanceOf(address(station));
        (Call[] memory list,) = driver.close(address(station), 1e18, 10_000);
        assertEq(list.length, 2, "an approval and a sale");
        for (uint256 i = 0; i < list.length; i++) {
            (bool ok, uint256 spent) =
                station.attempt(IDriver(address(driver)), list[i], StationCloses.ATTEMPT_GAS);
            assertTrue(ok, "a call failed within its allowance");
            console2.log("call / gas", i, spent);
        }
        uint256 sold = holding - IERC20(NVDA).balanceOf(safe);
        uint256 fetched = IERC20(USDG).balanceOf(address(station)) - home;
        console2.log("sold (1e18) / fetched (USDG, 6 dp)", sold, fetched);
        assertGt(sold, 0, "nothing sold");
        assertLt(sold, holding, "the limit did not stop the sale");
        assertGe(fetched * 1 ether / sold, _floor(feedPrice), "sold below the feed less its discount");
    }
}
