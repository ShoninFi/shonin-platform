// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {
    IAggregatorV3,
    IModuleProxyFactory,
    ISafe,
    ISafeProxyFactory,
    ISwapRouter02,
    IUniswapV3Pool
} from "../../src/drivers/robinhood/RobinhoodChain.sol";
import {RobinhoodDriver} from "../../src/drivers/robinhood/RobinhoodDriver.sol";
import {UniswapPrice} from "../../src/drivers/robinhood/UniswapPrice.sol";
import {Call} from "../../src/interfaces/IDriver.sol";
import {Test} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// The Robinhood driver's exit value sums the Safe's idle dollars and one
/// quote per stock held, each rounded down; a close sells each stock's
/// fraction rounded up and sends the idle fraction rounded down. So after a
/// close of a fraction the Safe reads above the stayers' share
/// (`exitBefore - freed`) by under a dollar-unit per figure summed: within
/// what the station lets a close leave for rounding, so a close inside the
/// band is never undone for it. The driver lists no more stocks than that
/// allowance covers.
///
/// Mocks stand in for the feed, the pools, the tokens and the Safe's
/// address; the sale is replayed exactly from the `amountIn` the driver's
/// own `close` calls carry and the `idle` transfer it adds.
contract AStockCloseRoundsByUnderAUnitPerFigureTest is Test {
    uint256 internal constant WAD = 1e18;
    uint32 internal constant WINDOW = 1800;

    MockToken internal usdg;
    MockToken internal nvda;
    MockToken internal aapl;
    MockPool internal nvdaPool;
    MockPool internal aaplPool;
    MockFeed internal nvdaFeed;
    MockFeed internal aaplFeed;
    RobinhoodDriver internal driver;
    address internal station = makeAddr("station");
    address internal safe;

    function setUp() public {
        usdg = new MockToken(6);
        nvda = new MockToken(18);
        aapl = new MockToken(18);
        // Ticks near the real ones (NVDA about $180, AAPL about $250 when
        // USDG is token0, as on Robinhood Chain); the sign follows the order.
        nvdaPool = _pool(address(nvda), 224_390);
        aaplPool = _pool(address(aapl), 221_100);
        // The feed agrees with the pool exactly, so nothing is outside the band.
        nvdaFeed = new MockFeed(_feedFromTick(nvdaPool.tick(), address(nvda)));
        aaplFeed = new MockFeed(_feedFromTick(aaplPool.tick(), address(aapl)));

        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](2);
        listings[0] = RobinhoodDriver.Listing(
            address(nvda),
            IAggregatorV3(address(nvdaFeed)),
            IUniswapV3Pool(address(nvdaPool)),
            type(uint256).max
        );
        listings[1] = RobinhoodDriver.Listing(
            address(aapl),
            IAggregatorV3(address(aaplFeed)),
            IUniswapV3Pool(address(aaplPool)),
            type(uint256).max
        );
        MockSafeFactory factory = new MockSafeFactory();
        driver = new RobinhoodDriver(
            RobinhoodDriver.Venue({
                dollar: IERC20(address(usdg)),
                router: makeAddr("router"),
                safeSingleton: makeAddr("singleton"),
                safeFactory: ISafeProxyFactory(address(factory)),
                rolesMastercopy: makeAddr("roles"),
                moduleFactory: IModuleProxyFactory(makeAddr("moduleFactory")),
                maxFeedAge: 4 days,
                twapWindow: WINDOW
            }),
            listings
        );
        safe = driver.safeOf(station);
        // The Safe exists (the driver only asks whether it has code).
        vm.etch(safe, hex"00");
    }

    function _pool(address token, int24 tickIfDollarFirst) internal returns (MockPool) {
        return address(usdg) < token
            ? new MockPool(address(usdg), token, tickIfDollarFirst)
            : new MockPool(token, address(usdg), -tickIfDollarFirst);
    }

    function _feedFromTick(int24 tick, address token) internal view returns (int256) {
        // Dollars (6 decimals) a whole token fetches at the tick, in the feed's 8.
        return int256(UniswapPrice.quoteAtTick(tick, 1e18, token, address(usdg)) * 100);
    }

    /// A constructed case, fraction 1% (a small claim): idle 99 units
    /// (0.99 of a unit lost to the floor of its fraction), and each stock
    /// holding worth q + 0.95 units at the TWAP with q ending in 90, so each
    /// sale's share floors away 0.90 more. The three floors lose 2.79 units
    /// where `freed` (one floor of the sum) loses 0.79: `exitAfter` lands two
    /// units above `exitBefore - freed`.
    function test_a_two_stock_close_with_every_figure_rounding_its_most_stays_within_a_unit_per_figure()
        public
    {
        uint256 fraction = 1e16;
        uint256 nvdaBalance = _balanceWorth(nvdaPool, address(nvda), 1_000_000_090, 95);
        uint256 aaplBalance = _balanceWorth(aaplPool, address(aapl), 2_000_000_090, 95);
        _assertWithinTolerance(nvdaBalance, aaplBalance, 99, fraction);
    }

    /// A balance whose worth at the pool's TWAP is `units + hundredths/100`
    /// dollar-units, to within the price of one wei of the token.
    function _balanceWorth(
        MockPool pool,
        address token,
        uint256 units,
        uint256 hundredths
    ) internal view returns (uint256 balance) {
        uint160 sqrtRatio = UniswapPrice.sqrtRatioAtTick(pool.tick());
        uint256 ratioX192 = uint256(sqrtRatio) * sqrtRatio;
        uint256 scaled = units * 100 + hundredths;
        balance = token < address(usdg)
            ? Math.mulDiv(scaled, 1 << 192, 100 * ratioX192)
            : Math.mulDiv(scaled, ratioX192, 100 * (1 << 192));
        assertEq(UniswapPrice.quoteAtTick(pool.tick(), balance, token, address(usdg)), units, "construction");
    }

    /// forge-config: default.fuzz.runs = 2000
    function testFuzz_a_two_stock_close_leaves_under_a_unit_per_figure_above_the_stayers_share(
        uint256 nvdaBalance,
        uint256 aaplBalance,
        uint256 idle,
        uint256 fraction
    ) public {
        nvdaBalance = bound(nvdaBalance, 1e17, 1e21);
        aaplBalance = bound(aaplBalance, 1e17, 1e21);
        idle = bound(idle, 0, 1e10);
        fraction = bound(fraction, 1e15, 9e17);
        _assertWithinTolerance(nvdaBalance, aaplBalance, idle, fraction);
    }

    function _assertWithinTolerance(
        uint256 nvdaBalance,
        uint256 aaplBalance,
        uint256 idle,
        uint256 fraction
    ) internal {
        nvda.mint(safe, nvdaBalance);
        aapl.mint(safe, aaplBalance);
        usdg.mint(safe, idle);

        (, uint256 exitBefore) = driver.valueOf(station);
        // StationCloses.close: what the fraction is worth at exit prices.
        uint256 freed = Math.mulDiv(exitBefore, fraction, WAD);

        // Replay the driver's own close: each sale's amountIn leaves the
        // Safe (sold through the router, paid to the station), and the idle
        // fraction is transferred to the station.
        (Call[] memory calls, bool blocked) = driver.close(station, fraction, 500);
        assertFalse(blocked, "in-band market must not block");
        for (uint256 i = 0; i < calls.length; i++) {
            (address to, bytes memory inner) = this.decodeExec(calls[i].data);
            if (to == driver.router()) {
                ISwapRouter02.ExactInputSingleParams memory p = this.decodeSwap(inner);
                MockToken(p.tokenIn).burn(safe, p.amountIn);
            } else if (to == address(usdg)) {
                (, uint256 amount) = this.decodeTransfer(inner);
                usdg.burn(safe, amount);
            } else if (to == address(nvda) || to == address(aapl)) {
                // The router's approval: moves nothing.
            } else {
                revert("unexpected call");
            }
        }

        (, uint256 exitAfter) = driver.valueOf(station);
        // Idle dollars and two quotes: three figures, each rounding by
        // under a unit.
        assertLe(exitAfter, exitBefore - freed + 3, "more than a unit per figure above the stayers' share");
        assertLe(
            exitAfter,
            exitBefore - freed + driver.MAX_LISTINGS() + 1,
            "past the station's allowance for rounding"
        );
    }

    function test_the_driver_lists_no_more_stocks_than_the_stations_rounding_allowance_covers() public {
        uint256 count = driver.MAX_LISTINGS() + 1;
        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](count);
        for (uint256 i = 0; i < count; i++) {
            MockToken token = new MockToken(18);
            MockPool pool = _pool(address(token), 224_390);
            listings[i] = RobinhoodDriver.Listing(
                address(token),
                IAggregatorV3(address(nvdaFeed)),
                IUniswapV3Pool(address(pool)),
                type(uint256).max
            );
        }
        RobinhoodDriver.Venue memory venue = RobinhoodDriver.Venue({
            dollar: IERC20(address(usdg)),
            router: makeAddr("router"),
            safeSingleton: makeAddr("singleton"),
            safeFactory: ISafeProxyFactory(address(new MockSafeFactory())),
            rolesMastercopy: makeAddr("roles"),
            moduleFactory: IModuleProxyFactory(makeAddr("moduleFactory")),
            maxFeedAge: 4 days,
            twapWindow: WINDOW
        });
        vm.expectRevert(RobinhoodDriver.TooMany.selector);
        new RobinhoodDriver(venue, listings);
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

contract MockToken {
    uint8 public immutable decimals;
    mapping(address => uint256) public balanceOf;

    constructor(uint8 decimals_) {
        decimals = decimals_;
    }

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function burn(address from, uint256 amount) external {
        balanceOf[from] -= amount;
    }

    /// The stock token's pause flag: never paused here.
    function oraclePaused() external pure returns (bool) {
        return false;
    }
}

contract MockFeed {
    int256 public answer;
    uint256 public age;

    constructor(int256 answer_) {
        answer = answer_;
    }

    function set(int256 answer_, uint256 age_) external {
        answer = answer_;
        age = age_;
    }

    function decimals() external pure returns (uint8) {
        return 8;
    }

    function latestRoundData() external view returns (uint80, int256, uint256, uint256, uint80) {
        return (1, answer, block.timestamp - age, block.timestamp - age, 1);
    }
}

/// A pool whose time-weighted tick is constant: `observe` answers a straight
/// line, as a quiet pool does between swaps.
contract MockPool {
    address public token0;
    address public token1;
    int24 public tick;

    constructor(address token0_, address token1_, int24 tick_) {
        token0 = token0_;
        token1 = token1_;
        tick = tick_;
    }

    function fee() external pure returns (uint24) {
        return 500;
    }

    function tickSpacing() external pure returns (int24) {
        return 10;
    }

    /// The current price, at the same tick.
    function slot0() external view returns (uint160, int24, uint16, uint16, uint16, uint8, bool) {
        return (UniswapPrice.sqrtRatioAtTick(tick), tick, 0, 1, 1, 0, true);
    }

    function observe(uint32[] calldata secondsAgos)
        external
        view
        returns (int56[] memory tickCumulatives, uint160[] memory liquidity)
    {
        tickCumulatives = new int56[](secondsAgos.length);
        liquidity = new uint160[](secondsAgos.length);
        for (uint256 i = 0; i < secondsAgos.length; i++) {
            tickCumulatives[i] = -int56(tick) * int56(uint56(secondsAgos[i]));
        }
    }
}

contract MockSafeFactory {
    function proxyCreationCode() external pure returns (bytes memory) {
        return hex"00";
    }
}
