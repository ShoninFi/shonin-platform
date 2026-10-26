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
import {
    IUniswapV3FactoryForSale,
    IUniswapV3PoolForSale,
    MintedToken
} from "./AFullBandSaleOnBaseFitsItsAllowance.t.sol";
import {MockFeed} from "./AStockCloseRoundsByUnderAUnitPerFigure.t.sol";
import {StationDouble} from "./StationDouble.sol";
import {Test, console2} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

interface IPoolTicks {
    function ticks(int24 tick)
        external
        view
        returns (
            uint128 liquidityGross,
            int128 liquidityNet,
            uint256 feeGrowthOutside0X128,
            uint256 feeGrowthOutside1X128,
            int56 tickCumulativeOutside,
            uint160 secondsPerLiquidityOutsideX128,
            uint32 secondsOutside,
            bool initialized
        );
    function feeGrowthGlobal0X128() external view returns (uint256);
    function feeGrowthGlobal1X128() external view returns (uint256);
}

/// The most a full-band sale costs, made as a stopped station's unwind
/// makes it (through the station's real Safe and Roles module, the real
/// router and a pool of Robinhood Chain's own Uniswap factory), walking
/// its whole distance with an initialised tick on every spacing that has
/// never been crossed with fees taken in both tokens: each crossing then
/// writes both fee slots from zero, the dearest a crossing can be. On
/// Robinhood Chain the dollar (USDG) is the pools' token0, so a stock
/// sale walks the price up, and every tick above the price is in that
/// state until something first crosses it (a laddered maker's range just
/// re-minted, or anyone's dust); the same state below the price, for a
/// sale walking down, is made here too. Each sale fits the station's
/// fixed allowance with at least a third of it to spare. Also the
/// mainnet listing (`maxSale` 1,000 NVDA) on the real NVDA/USDG pool.
/// Runs only with `ROBINHOOD_FORK_URL` (at the latest block: the public
/// endpoint keeps no old state). Nothing is sent to the chain.
///
/// forge-config: default.isolate = true
contract AFullBandSaleOnRobinhoodChainFitsItsAllowanceTest is Test {
    address internal constant USDG = 0x5fc5360D0400a0Fd4f2af552ADD042D716F1d168;
    address internal constant ROUTER = 0xCaf681a66D020601342297493863E78C959E5cb2;
    address internal constant FACTORY = 0x1f7d7550B1b028f7571E69A784071F0205FD2EfA;
    address internal constant NVDA = 0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC;
    address internal constant NVDA_POOL = 0xd4EB21209C4D6093f80B5b84f5C45cc093EA14a3;
    /// NVDA against USDG at the 1% fee (tick spacing 200): thin, so a sale
    /// of the real token walks its whole distance.
    address internal constant NVDA_POOL_1PCT = 0xc277560DF3689A401bA7deDd7626168b234Ceb5e;
    address internal constant NVDA_FEED = 0x379EC4f7C378F34a1B47E4F3cbeBCbAC3E8E9F15;
    address internal constant AAPL = 0xaF3D76f1834A1d425780943C99Ea8A608f8a93f9;
    address internal constant AAPL_POOL = 0xAae0d815EE56e4092a5E5C2911E676Fea50B2d6D;
    address internal constant AAPL_FEED = 0x6B22A786bAa607d76728168703a39Ea9C99f2cD0;
    address internal constant SAFE_SINGLETON = 0x29fcB43b46531BcA003ddC8FCB67FFE91900C762;
    address internal constant SAFE_FACTORY = 0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67;
    address internal constant ROLES_MASTERCOPY = 0x9646fDAD06d3e24444381f44362a3B0eB343D337;
    address internal constant MODULE_FACTORY = 0x000000000000aDdB49795b0f9bA5BC298cDda236;
    uint256 internal constant NVDA_MAX_SALE = 1000 ether;
    uint256 internal constant AAPL_MAX_SALE = 100 ether;
    uint32 internal constant WINDOW = 30 minutes;
    /// The most of its allowance one sale may spend: two thirds, so a
    /// third (916,667 gas, some twenty more of the dearest crossings) is
    /// left for a repricing of storage writes.
    uint256 internal constant MOST = StationCloses.ATTEMPT_GAS * 2 / 3;

    RobinhoodDriver internal driver;
    StationDouble internal station;
    address internal safe;
    bool internal forked;

    function setUp() public {
        string memory url = vm.envOr("ROBINHOOD_FORK_URL", string(""));
        if (bytes(url).length == 0) return;
        vm.createSelectFork(url);
        forked = true;
        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](2);
        listings[0] =
            RobinhoodDriver.Listing(NVDA, IAggregatorV3(NVDA_FEED), IUniswapV3Pool(NVDA_POOL), NVDA_MAX_SALE);
        listings[1] =
            RobinhoodDriver.Listing(AAPL, IAggregatorV3(AAPL_FEED), IUniswapV3Pool(AAPL_POOL), AAPL_MAX_SALE);
        driver = new RobinhoodDriver(_venue(), listings);
        station = new StationDouble(USDG, 100);
        station.approveKey(IDriver(address(driver)), makeAddr("executor"));
        safe = driver.safeOf(address(station));
        deal(USDG, address(this), 1_000_000_000e6);
        IERC20(USDG).approve(ROUTER, type(uint256).max);
    }

    modifier onFork() {
        if (!forked) {
            vm.skip(true);
            return;
        }
        _;
    }

    function _venue() internal pure returns (RobinhoodDriver.Venue memory) {
        return RobinhoodDriver.Venue({
            dollar: IERC20(USDG),
            router: ROUTER,
            safeSingleton: SAFE_SINGLETON,
            safeFactory: ISafeProxyFactory(SAFE_FACTORY),
            rolesMastercopy: ROLES_MASTERCOPY,
            moduleFactory: IModuleProxyFactory(MODULE_FACTORY),
            maxFeedAge: 4 days,
            twapWindow: WINDOW
        });
    }

    function uniswapV3MintCallback(uint256 amount0, uint256 amount1, bytes calldata) external {
        if (amount0 != 0) IERC20(IUniswapV3Pool(msg.sender).token0()).transfer(msg.sender, amount0);
        if (amount1 != 0) IERC20(IUniswapV3Pool(msg.sender).token1()).transfer(msg.sender, amount1);
    }

    /// One dust position on each of `count` spacings past `tick` in the
    /// direction `up`, from the first spacing boundary not behind it.
    function _dust(
        address pool,
        int24 tick,
        int24 spacing,
        uint256 count,
        bool up,
        uint128 amount
    ) internal {
        int24 floor = tick / spacing * spacing;
        if (tick < 0 && tick % spacing != 0) floor -= spacing;
        for (uint256 k = 0; k < count; k++) {
            int24 step = int24(int256(k)) * spacing;
            (int24 lower, int24 upper) = up
                ? (floor + spacing + step, floor + 2 * spacing + step)
                : (floor - spacing - step, floor - step);
            IUniswapV3PoolForSale(pool).mint(address(this), lower, upper, amount, "");
        }
    }

    /// How many initialised ticks the walk from `fromTick` to `toTick`
    /// crossed, and how many of those had never been crossed with fees in
    /// both tokens (both fee slots zero) before it, read from `before`.
    function _neverCrossed(
        address pool,
        int24 spacing,
        int24 low,
        int24 high
    ) internal view returns (uint256 initialised, uint256 dearest) {
        int24 first = low / spacing * spacing;
        for (int24 t = first; t <= high; t += spacing) {
            (,, uint256 f0, uint256 f1,,,, bool init) = IPoolTicks(pool).ticks(t);
            if (!init) continue;
            initialised++;
            if (f0 == 0 && f1 == 0) dearest++;
        }
    }

    /// A full-band close of everything `holder`'s Safe holds through
    /// `through`, each call with the station's allowance: whether every call
    /// went through, and the dearest call's gas.
    function _close(
        RobinhoodDriver through,
        StationDouble holder
    ) internal returns (bool allOk, uint256 worst, uint256 calls) {
        (Call[] memory list, bool blocked) = through.close(address(holder), 1e18, 10_000);
        assertTrue(blocked, "a full-band close that sells reports not done");
        allOk = true;
        calls = list.length;
        for (uint256 i = 0; i < list.length; i++) {
            (bool ok, uint256 spent) =
                holder.attempt(IDriver(address(through)), list[i], StationCloses.ATTEMPT_GAS);
            console2.log("call / ok / gas", i, ok, spent);
            if (!ok) allOk = false;
            if (spent > worst) worst = spent;
        }
    }

    function _thin(
        address stock,
        address pool
    ) internal returns (RobinhoodDriver thin, StationDouble holder) {
        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](1);
        listings[0] = RobinhoodDriver.Listing(
            stock, IAggregatorV3(address(new MockFeed(1e8))), IUniswapV3Pool(pool), type(uint128).max
        );
        thin = new RobinhoodDriver(_venue(), listings);
        holder = new StationDouble(USDG, 100);
        holder.approveKey(IDriver(address(thin)), makeAddr("thin executor"));
    }

    /// A plain token on the side of USDG's address `token1` asks for.
    function _plainToken(bool token1) internal returns (MintedToken stock) {
        for (uint256 i = 1; i < 256; i++) {
            stock = new MintedToken{salt: bytes32(i)}();
            if ((address(stock) > USDG) == token1) return stock;
        }
        revert("no address on that side");
    }

    /// A fresh 0.05% pool of a plain stock against USDG, fees taken in both
    /// tokens, and on every spacing of the sale's whole walk an initialised
    /// tick whose fee slots are both zero; the sale walks it to its limit.
    function _dearestWalk(bool up) internal returns (uint256 worst) {
        MintedToken stock = _plainToken(up);
        address pool = IUniswapV3FactoryForSale(FACTORY).createPool(USDG, address(stock), 500);
        int24 start = up ? int24(276_320) : int24(-276_320); // about a dollar a token
        IUniswapV3PoolForSale(pool).initialize(UniswapPrice.sqrtRatioAtTick(start));
        int24 spacing = 10;
        uint256 count = driver.SALE_SPACINGS() + 2;
        // Below the price a tick's fee slots start at the fee totals, so
        // they are zero only for a tick minted before any fee was taken.
        if (!up) _dust(pool, start - 5, spacing, count, false, 1e6);
        IUniswapV3PoolForSale(pool).mint(address(this), start - 1000, start + 1000, 1e17, "");
        // Fees in both tokens, the price kept within half a spacing.
        stock.approve(ROUTER, type(uint256).max);
        (address t0, address t1) = up ? (USDG, address(stock)) : (address(stock), USDG);
        ISwapRouter02(ROUTER).exactInputSingle(
            ISwapRouter02.ExactInputSingleParams(
                t0, t1, 500, address(this), 1e30, 0, UniswapPrice.sqrtRatioAtTick(start - 5)
            )
        );
        ISwapRouter02(ROUTER).exactInputSingle(
            ISwapRouter02.ExactInputSingleParams(
                t1, t0, 500, address(this), 1e30, 0, UniswapPrice.sqrtRatioAtTick(start + 5)
            )
        );
        assertGt(IPoolTicks(pool).feeGrowthGlobal0X128(), 0, "no fees in token0");
        assertGt(IPoolTicks(pool).feeGrowthGlobal1X128(), 0, "no fees in token1");
        // Above the price a tick's fee slots start at zero, whenever minted.
        if (up) _dust(pool, start + 5, spacing, count, true, 1e6);
        // A quiet half hour: the time-weighted price is where the pool is.
        vm.warp(vm.getBlockTimestamp() + WINDOW + 1);
        (RobinhoodDriver thin, StationDouble holder) = _thin(address(stock), pool);
        stock.transfer(thin.safeOf(address(holder)), 1e24);
        (, int24 before,,,,,) = IUniswapV3Pool(pool).slot0();
        int24 reach = int24(int256(thin.SALE_SPACINGS())) * spacing;
        (uint256 initialised, uint256 dearest) =
            _neverCrossed(pool, spacing, up ? before : before - reach, up ? before + reach : before);
        console2.log("initialised ticks on the walk / both fee slots zero", initialised, dearest);
        assertGe(dearest, thin.SALE_SPACINGS() - 1, "the walk is not the dearest");
        bool ok;
        (ok, worst,) = _close(thin, holder);
        (, int24 afterwards,,,,,) = IUniswapV3Pool(pool).slot0();
        int24 moved = up ? afterwards - before : before - afterwards;
        console2.log("ticks moved", int256(moved));
        console2.log("the dearest walk's sale through the Safe (gas)", worst);
        assertTrue(ok, "the sale failed within its allowance");
        assertGe(moved, reach - spacing, "the sale stopped short of its whole walk");
        (initialised, dearest) =
            _neverCrossed(pool, spacing, up ? before : before - reach, up ? before + reach : before);
        assertLe(dearest, 1, "the walk did not cross its ticks");
    }

    /// Uphill (the stock the pool's token1, as on Robinhood Chain's mainnet
    /// pools): a sale walking its whole distance through ticks never
    /// crossed, fees taken in both tokens, fits its allowance with a third
    /// to spare.
    function test_a_sale_walking_up_through_ticks_never_crossed_fits_its_allowance_with_a_third_to_spare()
        public
        onFork
    {
        uint256 worst = _dearestWalk(true);
        assertLt(worst, MOST, "the dearest walk up spends more than two thirds of its allowance");
    }

    /// Downhill (the stock the pool's token0, as WETH is on Base): the same.
    function test_a_sale_walking_down_through_ticks_never_crossed_fits_its_allowance_with_a_third_to_spare()
        public
        onFork
    {
        uint256 worst = _dearestWalk(false);
        assertLt(worst, MOST, "the dearest walk down spends more than two thirds of its allowance");
    }

    /// The real NVDA token (and the real USDG) on the real 1% NVDA/USDG
    /// pool, thin enough that the sale walks its whole distance, with a dust
    /// position on every spacing above the price (never crossed: both fee
    /// slots zero, both fee totals not): what the real tokens add to the
    /// dearest walk still leaves a third of the allowance.
    function test_a_sale_of_the_real_token_through_ticks_never_crossed_fits_its_allowance_with_a_third_to_spare(
    ) public onFork {
        address pool = NVDA_POOL_1PCT;
        int24 spacing = IUniswapV3Pool(pool).tickSpacing();
        (, int24 tick,,,,,) = IUniswapV3Pool(pool).slot0();
        _dust(pool, tick, spacing, driver.SALE_SPACINGS() + 2, true, 1e6);
        vm.warp(vm.getBlockTimestamp() + WINDOW + 1);
        (RobinhoodDriver thin, StationDouble holder) = _thin(NVDA, pool);
        deal(NVDA, thin.safeOf(address(holder)), 100_000 ether);
        int24 reach = int24(int256(thin.SALE_SPACINGS())) * spacing;
        (uint256 initialised, uint256 dearest) = _neverCrossed(pool, spacing, tick, tick + reach);
        console2.log("initialised ticks on the walk / both fee slots zero", initialised, dearest);
        (bool ok, uint256 worst,) = _close(thin, holder);
        (, int24 afterwards,,,,,) = IUniswapV3Pool(pool).slot0();
        console2.log("ticks moved", int256(afterwards - tick));
        console2.log("the real token's dearest walk through the Safe (gas)", worst);
        assertTrue(ok, "the sale failed within its allowance");
        assertGe(afterwards - tick, reach - spacing, "the sale stopped short of its whole walk");
        assertLt(worst, MOST, "the real token's dearest walk spends more than two thirds of its allowance");
    }

    /// The mainnet listing on the real NVDA/USDG pool as it is.
    function test_the_listed_maximum_sale_on_the_real_pool_fits_its_allowance_with_a_third_to_spare()
        public
        onFork
    {
        deal(NVDA, safe, NVDA_MAX_SALE);
        (bool ok, uint256 worst,) = _close(driver, station);
        assertTrue(ok, "the listed maximum's sale failed within its allowance");
        assertLt(worst, MOST, "the listed maximum's sale spends more than two thirds of its allowance");
    }

    /// The caller first pushes the real pool's price up, the way the sale
    /// walks, and mints dust on every spacing above: the sale still walks no
    /// further than its spacings from the time-weighted price, so it either
    /// isn't made (the pool already past its limit) or fits its allowance.
    function test_a_pool_pushed_the_sales_way_and_dusted_never_takes_the_sale_past_its_allowance()
        public
        onFork
    {
        deal(NVDA, safe, NVDA_MAX_SALE);
        deal(NVDA, address(this), 100_000 ether);
        IERC20(NVDA).approve(ROUTER, type(uint256).max);
        int24 spacing = IUniswapV3Pool(NVDA_POOL).tickSpacing();
        int24 reach = int24(int256(driver.SALE_SPACINGS())) * spacing;
        for (int24 push = reach / 2; push <= 2 * reach; push += reach / 2 * 3) {
            uint256 snap = vm.snapshotState();
            (, int24 tick0,,,,,) = IUniswapV3Pool(NVDA_POOL).slot0();
            ISwapRouter02(ROUTER).exactInputSingle(
                ISwapRouter02.ExactInputSingleParams(
                    NVDA,
                    USDG,
                    500,
                    address(this),
                    100_000 ether,
                    0,
                    UniswapPrice.sqrtRatioAtTick(tick0 + push)
                )
            );
            (, int24 tick,,,,,) = IUniswapV3Pool(NVDA_POOL).slot0();
            _dust(NVDA_POOL, tick, spacing, driver.SALE_SPACINGS() + 2, true, 1e6);
            uint256 held = IERC20(NVDA).balanceOf(safe);
            (Call[] memory list, bool blocked) = driver.close(address(station), 1e18, 10_000);
            assertTrue(blocked, "a close with stock left to sell said it was done");
            uint256 worst;
            for (uint256 i = 0; i < list.length; i++) {
                (bool ok, uint256 spent) =
                    station.attempt(IDriver(address(driver)), list[i], StationCloses.ATTEMPT_GAS);
                assertTrue(ok, "a call failed within its allowance");
                if (spent > worst) worst = spent;
            }
            (, int24 afterwards,,,,,) = IUniswapV3Pool(NVDA_POOL).slot0();
            console2.log("pushed (ticks) / calls / worst gas", uint256(int256(push)), list.length, worst);
            assertLt(worst, MOST, "the sale spent more than two thirds of its allowance");
            assertLe(afterwards - tick, reach + 1, "the sale walked past its spacings");
            if (list.length == 0) assertEq(IERC20(NVDA).balanceOf(safe), held, "something was sold");
            vm.revertToState(snap);
        }
    }

    /// Three closes in a row through the real token: each approve goes over
    /// what the last sale's limit left approved, and each still sells.
    function test_closes_in_a_row_through_the_real_token_approve_over_a_leftover_allowance() public onFork {
        deal(NVDA, safe, 3 * NVDA_MAX_SALE);
        for (uint256 i = 0; i < 3; i++) {
            (bool ok,,) = _close(driver, station);
            assertTrue(ok, "a close failed");
            // Arbitrage brings the price back and the half hour passes.
            vm.warp(vm.getBlockTimestamp() + WINDOW + 1);
        }
    }
}
