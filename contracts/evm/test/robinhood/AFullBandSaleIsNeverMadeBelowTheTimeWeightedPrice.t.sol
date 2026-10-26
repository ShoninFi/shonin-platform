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
import {Call, IDriver} from "../../src/interfaces/IDriver.sol";
import {StationCloses} from "../../src/station/StationCloses.sol";
import {IUniswapV3PoolForSale} from "./AFullBandSaleOnBaseFitsItsAllowance.t.sol";
import {StationDouble} from "./StationDouble.sol";
import {Test, console2} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// Anyone may send a stopped station's unwind, and at the full band its
/// sale has no minimum out, so it is the price limit that keeps the sale's
/// price: `SALE_SPACINGS` of the pool's tick spacings past its
/// time-weighted price, which no caller can move within the transaction
/// that sells. On Base's real WETH/USDC pool, forked at the latest block,
/// a caller who moves the pool before the sale (and back after), or who
/// moves it to just above that floor and supplies the liquidity the sale
/// sells into, never has WETH sold under the floor and, moving it past the
/// floor, has nothing sold at all. Runs only with `BASE_FORK_URL`; nothing
/// is sent to the chain.
contract AFullBandSaleIsNeverMadeBelowTheTimeWeightedPriceTest is Test {
    address internal constant USDC = 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913;
    address internal constant WETH = 0x4200000000000000000000000000000000000006;
    address internal constant ETH_FEED = 0x71041dddad3595F9CEd3DcCFBe3D1F4b0a16Bb70;
    address internal constant ROUTER = 0x2626664c2603336E57B271c5C0b26F421741e481;
    address internal constant WETH_POOL = 0xd0b53D9277642d899DF5C87A3966A349A798F224;
    address internal constant SAFE_SINGLETON = 0x29fcB43b46531BcA003ddC8FCB67FFE91900C762;
    address internal constant SAFE_FACTORY = 0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67;
    address internal constant ROLES_MASTERCOPY = 0x9646fDAD06d3e24444381f44362a3B0eB343D337;
    address internal constant MODULE_FACTORY = 0x000000000000aDdB49795b0f9bA5BC298cDda236;
    uint256 internal constant MAX_SALE = 300 ether;
    uint32 internal constant WINDOW = 30 minutes;
    int24 internal constant SPACING = 10;

    RobinhoodDriver internal driver;
    StationDouble internal station;
    address internal safe;
    bool internal forked;

    function setUp() public {
        string memory url = vm.envOr("BASE_FORK_URL", string(""));
        if (bytes(url).length == 0) return;
        vm.createSelectFork(url);
        forked = true;
        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](1);
        listings[0] =
            RobinhoodDriver.Listing(WETH, IAggregatorV3(ETH_FEED), IUniswapV3Pool(WETH_POOL), MAX_SALE);
        driver = new RobinhoodDriver(
            RobinhoodDriver.Venue({
                dollar: IERC20(USDC),
                router: ROUTER,
                safeSingleton: SAFE_SINGLETON,
                safeFactory: ISafeProxyFactory(SAFE_FACTORY),
                rolesMastercopy: ROLES_MASTERCOPY,
                moduleFactory: IModuleProxyFactory(MODULE_FACTORY),
                maxFeedAge: 1 hours,
                twapWindow: WINDOW
            }),
            listings
        );
        station = new StationDouble(USDC, 100);
        station.approveKey(IDriver(address(driver)), makeAddr("executor"));
        safe = driver.safeOf(address(station));
        deal(WETH, address(this), 100_000 ether);
        deal(USDC, address(this), 1_000_000_000e6);
        IERC20(WETH).approve(ROUTER, type(uint256).max);
        IERC20(USDC).approve(ROUTER, type(uint256).max);
    }

    modifier onFork() {
        if (!forked) {
            vm.skip(true);
            return;
        }
        _;
    }

    function uniswapV3MintCallback(uint256 amount0, uint256 amount1, bytes calldata) external {
        if (amount0 != 0) IERC20(WETH).transfer(msg.sender, amount0);
        if (amount1 != 0) IERC20(USDC).transfer(msg.sender, amount1);
    }

    /// The tick no sale may go under: `SALE_SPACINGS` spacings below the
    /// pool's time-weighted tick (WETH is token0, so a sale walks down).
    function _floorTick() internal view returns (int24) {
        return UniswapPrice.meanTick(IUniswapV3Pool(WETH_POOL), WINDOW)
            - int24(int256(driver.SALE_SPACINGS())) * SPACING;
    }

    /// The least `sold` WETH may fetch: at the floor, less the pool's fee.
    function _atTheFloor(uint256 sold) internal view returns (uint256) {
        return UniswapPrice.quoteAtTick(_floorTick(), sold, WETH, USDC) * (1e6 - 500) / 1e6;
    }

    function _swap(address tokenIn, address tokenOut, uint256 amount, uint160 limit) internal {
        ISwapRouter02(ROUTER).exactInputSingle(
            ISwapRouter02.ExactInputSingleParams(tokenIn, tokenOut, 500, address(this), amount, 0, limit)
        );
    }

    /// One full-band close as `closeAll` makes it: what it sold and fetched.
    function _close() internal returns (uint256 sold, uint256 fetched, uint256 calls) {
        uint256 held = IERC20(WETH).balanceOf(safe);
        uint256 home = IERC20(USDC).balanceOf(address(station));
        (Call[] memory list, bool blocked) = driver.close(address(station), 1e18, 10_000);
        assertTrue(blocked, "a close with WETH left to sell said it was done");
        calls = list.length;
        for (uint256 i = 0; i < list.length; i++) {
            (bool ok,) = station.attempt(IDriver(address(driver)), list[i], StationCloses.ATTEMPT_GAS);
            assertTrue(ok, "a call of the sale failed within its allowance");
        }
        sold = held - IERC20(WETH).balanceOf(safe);
        fetched = IERC20(USDC).balanceOf(address(station)) - home;
        console2.log("sold WETH (wei) / fetched USDC (6 dp)", sold, fetched);
    }

    /// The caller's gain at the pool's price before anything moved.
    function _gain(uint256 wethStart, uint256 usdcStart, uint256 ethPrice) internal view returns (int256) {
        int256 wethDelta = int256(IERC20(WETH).balanceOf(address(this))) - int256(wethStart);
        int256 usdcDelta = int256(IERC20(USDC).balanceOf(address(this))) - int256(usdcStart);
        return usdcDelta + wethDelta * int256(ethPrice) / 1e18;
    }

    /// With the pool as it is, the sale sells and fetches no less than the
    /// floor, and near the time-weighted price.
    function test_with_the_pool_untouched_the_sale_fetches_near_the_time_weighted_price() public onFork {
        deal(WETH, safe, MAX_SALE);
        (, uint256 fair) = driver.valueOf(address(station));
        (uint256 sold, uint256 fetched,) = _close();
        assertGt(sold, 0, "an untouched pool took no sale");
        assertGe(fetched, _atTheFloor(sold), "the sale fetched less than the floor");
        assertGe(fetched, fair * sold / MAX_SALE * 95 / 100, "the sale fetched far under the TWAP");
    }

    /// A caller who sells WETH to move the pool down past the floor before
    /// the sale, and buys back after, has nothing sold, and loses the
    /// round trip's fees. Before the time-weighted anchor the sale followed
    /// the moved price and the caller took the difference.
    function test_a_caller_who_moves_the_pool_past_the_floor_first_has_nothing_sold() public onFork {
        int24[2] memory pushes = [int24(3000), 7000];
        for (uint256 p = 0; p < pushes.length; p++) {
            uint256 snap = vm.snapshotState();
            deal(WETH, safe, MAX_SALE);
            (uint160 sqrtBefore, int24 tickBefore,,,,,) = IUniswapV3Pool(WETH_POOL).slot0();
            uint256 ethPrice = UniswapPrice.quoteAtTick(tickBefore, 1 ether, WETH, USDC);
            uint256 wethStart = IERC20(WETH).balanceOf(address(this));
            uint256 usdcStart = IERC20(USDC).balanceOf(address(this));
            _swap(WETH, USDC, 100_000 ether, UniswapPrice.sqrtRatioAtTick(tickBefore - pushes[p]));
            (uint256 sold, uint256 fetched,) = _close();
            _swap(USDC, WETH, 1_000_000_000e6, sqrtBefore);
            int256 gain = _gain(wethStart, usdcStart, ethPrice);
            console2.log("pushed (ticks)", uint256(int256(pushes[p])));
            console2.log("the caller's gain (USDC, 6 dp)", gain);
            assertEq(sold, 0, "WETH was sold past the floor");
            assertEq(fetched, 0);
            assertLe(gain, 0, "the caller profited from the station's sale");
            vm.revertToState(snap);
        }
    }

    /// A caller who moves the pool to just above the floor and supplies the
    /// liquidity the sale sells into, then has the station sell three times
    /// in the one transaction: every sale is made at or above the floor,
    /// so what the caller can take is bounded by the floor's distance from
    /// the time-weighted price, however much the Safe holds.
    function test_a_caller_who_supplies_the_liquidity_at_a_moved_price_buys_at_no_less_than_the_floor()
        public
        onFork
    {
        deal(WETH, safe, 3 * MAX_SALE);
        int24 floorTick = _floorTick();
        int24 lower = floorTick / SPACING * SPACING;
        if (floorTick < 0 && floorTick % SPACING != 0) lower -= SPACING;
        lower += SPACING;
        int24 target = lower + 2 * SPACING;
        _swap(WETH, USDC, 100_000 ether, UniswapPrice.sqrtRatioAtTick(target));
        (, int24 tick,,,,,) = IUniswapV3Pool(WETH_POOL).slot0();
        console2.log("pushed to tick", int256(tick));
        console2.log("floor tick", int256(floorTick));
        // Deep liquidity of the caller's own, from the floor to the price.
        IUniswapV3PoolForSale(WETH_POOL).mint(address(this), lower, target, 1e20, "");
        uint256 soldAll;
        for (uint256 i = 0; i < 3; i++) {
            (uint256 sold, uint256 fetched,) = _close();
            soldAll += sold;
            assertGe(fetched, _atTheFloor(sold), "a sale fetched less than the floor");
        }
        (, int24 afterwards,,,,,) = IUniswapV3Pool(WETH_POOL).slot0();
        assertGe(afterwards, floorTick - 1, "the pool went under the floor");
        console2.log("sold in the one transaction (wei)", soldAll);
    }

    /// A caller who moves the pool up first (the price above its
    /// time-weighted one, against the sale's way): the sale walks no
    /// further than its spacings from where the pool is, at prices above
    /// the time-weighted one, within its allowance.
    function test_a_caller_who_moves_the_pool_up_first_gets_a_sale_above_the_time_weighted_price()
        public
        onFork
    {
        deal(WETH, safe, MAX_SALE);
        (, int24 tick0,,,,,) = IUniswapV3Pool(WETH_POOL).slot0();
        _swap(USDC, WETH, 1_000_000_000e6, UniswapPrice.sqrtRatioAtTick(tick0 + 1000));
        (, int24 tick,,,,,) = IUniswapV3Pool(WETH_POOL).slot0();
        int24 mean = UniswapPrice.meanTick(IUniswapV3Pool(WETH_POOL), WINDOW);
        (uint256 sold, uint256 fetched,) = _close();
        (, int24 afterwards,,,,,) = IUniswapV3Pool(WETH_POOL).slot0();
        int24 reach = int24(int256(driver.SALE_SPACINGS())) * SPACING;
        assertGt(sold, 0, "nothing was sold");
        assertGe(afterwards, tick - reach - 1, "the sale walked past its spacings from the pool's price");
        assertGe(
            fetched,
            UniswapPrice.quoteAtTick(mean, sold, WETH, USDC),
            "the sale fetched less than the time-weighted price"
        );
    }
}
