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
import {MockFeed, MockSafeFactory, MockToken} from "./AStockCloseRoundsByUnderAUnitPerFigure.t.sol";
import {Test} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A pool whose price now and whose time-weighted price over the window
/// are set apart, as after someone moved it within the transaction; its
/// `observe` can be made to fail, as a pool's does when its observations
/// don't reach back over the window.
contract SkewedPool {
    address public token0;
    address public token1;
    int24 public tick;
    int24 public mean;
    bool public unreadable;

    constructor(address token0_, address token1_, int24 tick_) {
        token0 = token0_;
        token1 = token1_;
        tick = tick_;
        mean = tick_;
    }

    function set(int24 tick_, int24 mean_, bool unreadable_) external {
        tick = tick_;
        mean = mean_;
        unreadable = unreadable_;
    }

    function fee() external pure returns (uint24) {
        return 500;
    }

    function tickSpacing() external pure returns (int24) {
        return 10;
    }

    function slot0() external view returns (uint160, int24, uint16, uint16, uint16, uint8, bool) {
        return (UniswapPrice.sqrtRatioAtTick(tick), tick, 0, 1, 1, 0, true);
    }

    function observe(uint32[] calldata secondsAgos)
        external
        view
        returns (int56[] memory tickCumulatives, uint160[] memory liquidity)
    {
        require(!unreadable, "OLD");
        tickCumulatives = new int56[](secondsAgos.length);
        liquidity = new uint160[](secondsAgos.length);
        for (uint256 i = 0; i < secondsAgos.length; i++) {
            tickCumulatives[i] = -int56(mean) * int56(uint56(secondsAgos[i]));
        }
    }
}

/// Anyone may send a stopped station's unwind, and at the full band the
/// sale has no minimum out. So its price limit is `SALE_SPACINGS` of the
/// pool's tick spacings past the nearer of the pool's price and its
/// time-weighted price: never past the time-weighted price's bound (which
/// no caller can move within the transaction), and never further from
/// where the pool is than those spacings (which bounds the sale's gas).
/// A pool already at or past the time-weighted bound, or whose
/// time-weighted price can't be read, gets no sale (the close says it
/// isn't done, and the idle dollars still go), and the next unwind tries
/// again. The feeds here are at the pools' price, so their own bound (the
/// feed less its discount) is further off than the time-weighted one,
/// except where a test says otherwise.
contract AFullBandSaleLimitIsAnchoredToTheTimeWeightedPriceTest is Test {
    uint256 internal constant WAD = 1e18;
    uint32 internal constant WINDOW = 1800;
    uint256 internal constant MAX = 50e18;
    int24 internal constant SPACING = 10;

    MockToken internal usdg;
    /// A stock the pool lists first (a sale walks the price down) and one
    /// it lists second (a sale walks it up).
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
        usdg = new MockToken(6);
        MockToken a = new MockToken(18);
        MockToken b = new MockToken(18);
        (MockToken first, MockToken second) = address(a) < address(b) ? (a, b) : (b, a);
        // Fresh tokens until one sits below the dollar's address and one above.
        while (address(first) > address(usdg)) first = new MockToken(18);
        while (address(second) < address(usdg)) second = new MockToken(18);
        low = first;
        high = second;
        lowPool = new SkewedPool(address(low), address(usdg), -224_390);
        highPool = new SkewedPool(address(usdg), address(high), 224_390);
        // About the pools' price at tick 224,390: $180.03.
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
        low.mint(safe, MAX);
        high.mint(safe, MAX);
        usdg.mint(safe, 1000e6);
    }

    /// Each token's full-band sale in a close: its price limit (zero when
    /// no sale is made), and whether the idle dollars were sent.
    function _limits() internal view returns (uint160 lowLimit, uint160 highLimit, bool idle, bool blocked) {
        Call[] memory calls;
        (calls, blocked) = driver.close(station, WAD, 10_000);
        for (uint256 i = 0; i < calls.length; i++) {
            (address to, bytes memory inner) = this.decodeExec(calls[i].data);
            if (to == address(usdg)) idle = true;
            if (to != driver.router()) continue;
            ISwapRouter02.ExactInputSingleParams memory p = this.decodeSwap(inner);
            if (p.tokenIn == address(low)) lowLimit = p.sqrtPriceLimitX96;
            else highLimit = p.sqrtPriceLimitX96;
        }
    }

    /// Moved the sale's way but not past the bound: the limit is the
    /// time-weighted bound, not `SALE_SPACINGS` past the moved price.
    function test_a_pool_moved_the_sales_way_is_sold_no_further_than_the_time_weighted_bound() public {
        int24 mean = 224_390;
        lowPool.set(-mean - reach / 2, -mean, false);
        highPool.set(mean + reach / 2, mean, false);
        (uint160 lowLimit, uint160 highLimit,, bool blocked) = _limits();
        assertTrue(blocked);
        assertEq(lowLimit, UniswapPrice.sqrtRatioAtTick(-mean - reach), "walking down, past the bound");
        assertEq(highLimit, UniswapPrice.sqrtRatioAtTick(mean + reach), "walking up, past the bound");
    }

    /// Moved the sale's way to the bound or past it: no sale, the close not
    /// done, the idle dollars still sent.
    function test_a_pool_moved_past_the_time_weighted_bound_gets_no_sale() public {
        int24 mean = 224_390;
        int24[3] memory pushes = [reach, reach + 1, 30 * reach];
        for (uint256 i = 0; i < pushes.length; i++) {
            lowPool.set(-mean - pushes[i], -mean, false);
            highPool.set(mean + pushes[i], mean, false);
            (uint160 lowLimit, uint160 highLimit, bool idle, bool blocked) = _limits();
            assertEq(lowLimit, 0, "a sale walking down past the bound was made");
            assertEq(highLimit, 0, "a sale walking up past the bound was made");
            assertTrue(idle, "the idle dollars were held with the sales");
            assertTrue(blocked, "a close leaving stock unsold said it was done");
        }
    }

    /// Moved against the sale's way: the limit is `SALE_SPACINGS` from
    /// where the pool is, so the walk (and its gas) stays bounded, and
    /// every price on it is better than the time-weighted one.
    function test_a_pool_moved_against_the_sales_way_is_walked_no_further_than_its_spacings() public {
        int24 mean = 224_390;
        int24 push = 30 * reach;
        lowPool.set(-mean + push, -mean, false);
        highPool.set(mean - push, mean, false);
        (uint160 lowLimit, uint160 highLimit,,) = _limits();
        assertEq(
            lowLimit, UniswapPrice.sqrtRatioAtTick(-mean + push - reach), "walking down from a raised pool"
        );
        assertEq(
            highLimit, UniswapPrice.sqrtRatioAtTick(mean - push + reach), "walking up from a lowered pool"
        );
    }

    /// A pool whose time-weighted price can't be read gets no sale; the
    /// other token is still sold, and the idle dollars still go.
    function test_a_pool_whose_time_weighted_price_cannot_be_read_gets_no_sale() public {
        lowPool.set(-224_390, -224_390, true);
        (uint160 lowLimit, uint160 highLimit, bool idle, bool blocked) = _limits();
        assertEq(lowLimit, 0, "a sale was made with no time-weighted price");
        assertEq(highLimit, UniswapPrice.sqrtRatioAtTick(224_390 + reach), "the other token's sale");
        assertTrue(idle);
        assertTrue(blocked);
    }

    /// Near the tick limits the bound is the pool's own (with no feed
    /// answer to bound it first).
    function test_near_the_tick_limits_the_limit_is_the_pools_own_bound() public {
        lowFeed.set(0, 0);
        highFeed.set(0, 0);
        lowPool.set(-887_272 + reach / 2, -887_272 + reach / 2, false);
        highPool.set(887_272 - reach / 2, 887_272 - reach / 2, false);
        (uint160 lowLimit, uint160 highLimit,,) = _limits();
        assertEq(lowLimit, 4_295_128_739 + 1);
        assertEq(highLimit, 1_461_446_703_485_210_103_287_273_052_203_988_822_378_723_970_342 - 1);
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
