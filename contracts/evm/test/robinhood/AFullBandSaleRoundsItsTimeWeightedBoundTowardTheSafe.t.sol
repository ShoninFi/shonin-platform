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

/// A pool whose tick cumulative over the window is set directly, so its
/// mean can be fractional (as a real pool's almost always is).
contract FractionalMeanPool {
    address public token0;
    address public token1;
    int24 public tick;
    /// The tick cumulative's change over the window.
    int56 public delta;

    constructor(address token0_, address token1_) {
        token0 = token0_;
        token1 = token1_;
    }

    function set(int24 tick_, int56 delta_) external {
        tick = tick_;
        delta = delta_;
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
        tickCumulatives = new int56[](secondsAgos.length);
        liquidity = new uint160[](secondsAgos.length);
        // [window ago, now]: now - then = delta.
        tickCumulatives[0] = 0;
        tickCumulatives[1] = delta;
    }
}

/// A full-band sale stops `SALE_SPACINGS` of its pool's tick spacings past
/// the pool's time-weighted tick (when that is nearer than the current
/// one). A real pool's mean tick over the window is almost always
/// fractional, so it has to be rounded to a whole tick, and it is rounded
/// towards the Safe in both directions: up for a sale walking the price
/// down (the stock is the pool's token0), down for one walking it up (the
/// stock is token1). So the sale's bound is never past the time-weighted
/// price less its spacings, not even by a tick. The exit value read from the
/// same mean rounds towards the lower worth in both token orders.
contract AFullBandSaleRoundsItsTimeWeightedBoundTowardTheSafeTest is Test {
    uint256 internal constant WAD = 1e18;
    uint32 internal constant WINDOW = 1800;
    int24 internal constant SPACING = 10;

    MockToken internal usdc;
    MockToken internal weth;
    FractionalMeanPool internal pool;
    RobinhoodDriver internal driver;
    address internal station = makeAddr("station");
    address internal safe;

    /// A driver listing `weth` against `usdc`, with `weth` the pool's
    /// token0 when `stockFirst`.
    function _make(bool stockFirst) internal {
        usdc = new MockToken(6);
        weth = new MockToken(18);
        while ((address(weth) < address(usdc)) != stockFirst) weth = new MockToken(18);
        pool = stockFirst
            ? new FractionalMeanPool(address(weth), address(usdc))
            : new FractionalMeanPool(address(usdc), address(weth));
        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](1);
        listings[0] = RobinhoodDriver.Listing(
            address(weth), IAggregatorV3(address(new MockFeed(2300e8))), IUniswapV3Pool(address(pool)), 300e18
        );
        driver = new RobinhoodDriver(
            RobinhoodDriver.Venue({
                dollar: IERC20(address(usdc)),
                router: makeAddr("router"),
                safeSingleton: makeAddr("singleton"),
                safeFactory: ISafeProxyFactory(address(new MockSafeFactory())),
                rolesMastercopy: makeAddr("roles"),
                moduleFactory: IModuleProxyFactory(makeAddr("moduleFactory")),
                maxFeedAge: 1 hours,
                twapWindow: WINDOW
            }),
            listings
        );
        safe = driver.safeOf(station);
        vm.etch(safe, hex"00");
        weth.mint(safe, 300e18);
    }

    function decodeExec(bytes calldata data) external pure returns (address to, bytes memory inner) {
        (to,, inner,,,,,,,) = abi.decode(
            data[4:], (address, uint256, bytes, uint8, uint256, uint256, uint256, address, address, bytes)
        );
    }

    function decodeSwap(bytes calldata data)
        external
        pure
        returns (ISwapRouter02.ExactInputSingleParams memory)
    {
        return abi.decode(data[4:], (ISwapRouter02.ExactInputSingleParams));
    }

    function _limit() internal view returns (uint160 limit) {
        (Call[] memory calls,) = driver.close(station, WAD, 10_000);
        for (uint256 i = 0; i < calls.length; i++) {
            (address to, bytes memory inner) = this.decodeExec(calls[i].data);
            if (to == driver.router()) limit = this.decodeSwap(inner).sqrtPriceLimitX96;
        }
    }

    /// A tick cumulative over the window whose mean is `whole` and a half.
    function _halfPast(int24 whole) internal pure returns (int56) {
        return int56(whole) * int56(uint56(WINDOW)) + int56(uint56(WINDOW / 2));
    }

    function _reach() internal view returns (int24) {
        return int24(int256(driver.SALE_SPACINGS())) * SPACING;
    }

    /// A sale walking the price down (the stock is token0, as WETH on
    /// Base), with the pool pushed below its mean so the time-weighted bound
    /// is the nearer: the floor is at or above the mean less its spacings,
    /// for a negative mean and a positive one.
    function test_a_sale_walking_down_is_never_floored_below_the_time_weighted_price_less_its_spacings()
        public
    {
        _make(true);
        int24[2] memory means = [int24(-198_861), int24(198_860)];
        for (uint256 i = 0; i < means.length; i++) {
            // A mean of means[i] + 0.5, rounded towards the Safe: means[i] + 1.
            pool.set(means[i] - 40, _halfPast(means[i]));
            uint160 limit = _limit();
            assertGt(limit, 0, "no sale");
            assertEq(
                limit,
                UniswapPrice.sqrtRatioAtTick(means[i] + 1 - _reach()),
                "the floor is below the time-weighted price less its spacings"
            );
        }
    }

    /// A sale walking the price up (the stock is token1, as on Robinhood
    /// Chain, where the dollar is token0), with the pool pushed above its
    /// mean: the bound is at or below the mean plus its spacings.
    function test_a_sale_walking_up_is_never_bounded_past_the_time_weighted_price_plus_its_spacings()
        public
    {
        _make(false);
        int24[2] memory means = [int24(-198_861), int24(198_860)];
        for (uint256 i = 0; i < means.length; i++) {
            // A mean of means[i] + 0.5, rounded towards the Safe: means[i].
            pool.set(means[i] + 40, _halfPast(means[i]));
            uint160 limit = _limit();
            assertGt(limit, 0, "no sale");
            assertEq(
                limit,
                UniswapPrice.sqrtRatioAtTick(means[i] + _reach()),
                "the bound is past the time-weighted price plus its spacings"
            );
        }
    }

    /// The exit value at a fractional mean is the lower of the two whole
    /// ticks around it, in both token orders: never more than the holding
    /// is worth at the pool's time-weighted price.
    function test_the_exit_value_at_a_fractional_mean_is_the_lower_of_its_whole_ticks() public {
        for (uint256 order = 0; order < 2; order++) {
            bool stockFirst = order == 0;
            _make(stockFirst);
            int24 whole = stockFirst ? int24(-198_861) : int24(198_860);
            pool.set(whole, _halfPast(whole));
            (, uint256 exit) = driver.valueOf(station);
            uint256 below = UniswapPrice.quoteAtTick(whole, 300e18, address(weth), address(usdc));
            uint256 above = UniswapPrice.quoteAtTick(whole + 1, 300e18, address(weth), address(usdc));
            assertGt(exit, 0, "no exit value");
            assertEq(exit, below < above ? below : above, "the exit value rounds up");
        }
    }
}
