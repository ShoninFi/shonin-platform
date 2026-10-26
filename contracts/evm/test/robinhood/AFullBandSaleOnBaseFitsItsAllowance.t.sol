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
import {MockFeed} from "./AStockCloseRoundsByUnderAUnitPerFigure.t.sol";
import {StationDouble} from "./StationDouble.sol";
import {Test, console2} from "forge-std/Test.sol";
import {ERC20} from "openzeppelin/token/ERC20/ERC20.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

interface IUniswapV3FactoryForSale {
    function createPool(address tokenA, address tokenB, uint24 fee) external returns (address pool);
}

interface IUniswapV3PoolForSale {
    function initialize(uint160 sqrtPriceX96) external;
    function mint(
        address recipient,
        int24 tickLower,
        int24 tickUpper,
        uint128 amount,
        bytes calldata data
    ) external returns (uint256 amount0, uint256 amount1);
}

contract MintedToken is ERC20 {
    constructor() ERC20("M", "M") {
        _mint(msg.sender, type(uint128).max);
    }
}

/// A full-band sale on Base, as a stopped station's unwind makes it: the
/// `uniswap-base` listing (WETH against Circle's USDC in Uniswap v3's 0.05%
/// pool) sells at most its maximum per close, and that sale, made through
/// the station's real Safe (v1.4.1, with its Zodiac Roles v2 module set up)
/// into the real pool at any price, spends well under the fixed allowance
/// the station gives each call. A sale into a thin pool with every tick
/// spacing on its side initialised (range boundaries, or anyone's dust
/// positions) stops at its price limit within the allowance, and every
/// retry sells on from where the last stopped. Runs only with
/// `BASE_FORK_URL` set (a Base mainnet RPC); skipped otherwise. Nothing is
/// sent to the chain.
contract AFullBandSaleOnBaseFitsItsAllowanceTest is Test {
    address internal constant USDC = 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913;
    address internal constant WETH = 0x4200000000000000000000000000000000000006;
    address internal constant ETH_FEED = 0x71041dddad3595F9CEd3DcCFBe3D1F4b0a16Bb70;
    address internal constant ROUTER = 0x2626664c2603336E57B271c5C0b26F421741e481;
    address internal constant WETH_POOL = 0xd0b53D9277642d899DF5C87A3966A349A798F224;
    address internal constant FACTORY = 0x33128a8fC17869897dcE68Ed026d694621f6FDfD;
    address internal constant SAFE_SINGLETON = 0x29fcB43b46531BcA003ddC8FCB67FFE91900C762;
    address internal constant SAFE_FACTORY = 0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67;
    address internal constant ROLES_MASTERCOPY = 0x9646fDAD06d3e24444381f44362a3B0eB343D337;
    address internal constant MODULE_FACTORY = 0x000000000000aDdB49795b0f9bA5BC298cDda236;
    /// The listing's maximum sale.
    uint256 internal constant MAX_SALE = 300 ether;
    /// What the Safe's own transaction adds around a call, at the most.
    uint256 internal constant SAFE_AROUND = 100_000;

    RobinhoodDriver internal driver;
    StationDouble internal station;
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
                twapWindow: 30 minutes
            }),
            listings
        );
        station = new StationDouble(USDC, 500);
        // The station's Safe and Roles module, set up as a key's approval
        // sets them up.
        station.approveKey(IDriver(address(driver)), makeAddr("executor"));
    }

    modifier onFork() {
        if (!forked) {
            vm.skip(true);
            return;
        }
        _;
    }

    /// A Safe holding more than the maximum: the close sells no more than
    /// the maximum, says it isn't done, and every call, the sale included, goes
    /// through with the allowance and spends well under it.
    function test_one_bounded_sale_through_the_safe_fits_its_allowance_with_room_to_spare() public onFork {
        address safe = driver.safeOf(address(station));
        deal(WETH, safe, 3 * MAX_SALE);
        deal(USDC, safe, 1000e6);
        uint256 before = gasleft();
        driver.valueOf(address(station));
        (Call[] memory calls, bool blocked) = driver.close(address(station), 1e18, 10_000);
        driver.targets();
        uint256 reads = before - gasleft();
        console2.log("the driver's reads for one listing (gas)", reads);
        assertTrue(blocked, "a close leaving WETH unsold reported itself done");
        assertEq(calls.length, 3);
        uint256 worst;
        for (uint256 i = 0; i < calls.length; i++) {
            (bool ok, uint256 spent) =
                station.attempt(IDriver(address(driver)), calls[i], StationCloses.ATTEMPT_GAS);
            assertTrue(ok, "a call of the bounded sale failed within its allowance");
            console2.log("call / gas", i, spent);
            if (spent > worst) worst = spent;
        }
        // At most the maximum: the price limit may stop the sale sooner.
        assertGe(IERC20(WETH).balanceOf(safe), 2 * MAX_SALE, "more than the maximum was sold");
        assertLt(IERC20(WETH).balanceOf(safe), 3 * MAX_SALE, "nothing was sold");
        assertGt(
            IERC20(USDC).balanceOf(address(station)), 1000e6, "the sale's dollars did not reach the station"
        );
        assertLt(worst, StationCloses.ATTEMPT_GAS / 2, "the bounded sale is not well under its allowance");
        // The driver's reads at its most listings stay under what the
        // worst full unwind is measured with (700,000) and what the part's
        // floor keeps for them.
        assertLt(reads * driver.MAX_LISTINGS(), 700_000, "the driver's reads outgrow the worst full unwind's");
        assertLt(
            reads * driver.MAX_LISTINGS(),
            StationCloses.PART_FLOOR - StationCloses.ATTEMPT_FLOOR,
            "the driver's reads outgrow the part's floor"
        );
    }

    /// A sale into a pool whose liquidity runs out walks the tick bitmap to
    /// the tick limit, one word (256 ticks * spacing) per step: the most a
    /// sale costs in a pool nobody has filled with ticks. Fee 500 is what
    /// the mainnet pools use (spacing 10); fee 100 (spacing 1) is what a
    /// listing on another pool could use.
    function test_a_sale_into_an_emptied_pool_fits_its_allowance() public onFork {
        uint24[2] memory fees = [uint24(500), 100];
        for (uint256 f = 0; f < fees.length; f++) {
            uint256 snap = vm.snapshotState();
            MintedToken a = new MintedToken();
            MintedToken b = new MintedToken();
            (address t0, address t1) =
                address(a) < address(b) ? (address(a), address(b)) : (address(b), address(a));
            address pool = IUniswapV3FactoryForSale(FACTORY).createPool(t0, t1, fees[f]);
            // About tick -196,000: a WETH/USDC-like price (18 against 6 decimals).
            IUniswapV3PoolForSale(pool).initialize(uint160(4_390_000_000_000_000_000_000_000));
            int24 spacing = fees[f] == 500 ? int24(10) : int24(1);
            IUniswapV3PoolForSale(pool).mint(
                address(this), -196_500 / spacing * spacing, -195_500 / spacing * spacing, 1e15, ""
            );
            IERC20(t0).approve(ROUTER, type(uint256).max);
            uint256 before = gasleft();
            ISwapRouter02(ROUTER).exactInputSingle(
                ISwapRouter02.ExactInputSingleParams({
                    tokenIn: t0,
                    tokenOut: t1,
                    fee: fees[f],
                    recipient: address(this),
                    amountIn: 1_000_000 ether,
                    amountOutMinimum: 0,
                    sqrtPriceLimitX96: 0
                })
            );
            uint256 gas = before - gasleft();
            console2.log("fee / gas to the tick limit", uint256(fees[f]), gas);
            if (fees[f] == 500) {
                assertLt(
                    gas + SAFE_AROUND,
                    StationCloses.ATTEMPT_GAS,
                    "a sale into an emptied pool outgrows its allowance"
                );
            }
            vm.revertToState(snap);
        }
    }

    /// A thin 0.05% pool of a stock against USDC, its liquidity spread over
    /// every tick spacing on the sale's side for far more than one sale's
    /// distance, each spacing its own initialised position. A close at the
    /// full band of more than the pool can take: each retry's sale goes
    /// through within its allowance with room to spare, moves the price no
    /// further than its limit, sells some, and brings dollars home, and the
    /// close says it isn't done. Without the limit the same sale would walk
    /// across every one of those ticks and outgrow its allowance.
    function test_a_sale_into_a_pool_with_every_tick_on_its_side_initialised_progresses_within_its_allowance()
        public
        onFork
    {
        MintedToken stock = new MintedToken();
        address pool = IUniswapV3FactoryForSale(FACTORY).createPool(address(stock), USDC, 500);
        bool down = address(stock) < USDC;
        int24 start = -276_320; // about $1 for a whole stock token, either way round
        if (!down) start = -start;
        IUniswapV3PoolForSale(pool).initialize(UniswapPrice.sqrtRatioAtTick(start));
        deal(USDC, address(this), 1e15);
        // Every spacing on the sale's side, for 300 spacings: the price's
        // path down (up) the ticks, each with its own position.
        int24 spacing = 10;
        for (int24 k = 0; k < 300; k++) {
            int24 near = down ? start - k * spacing : start + k * spacing;
            int24 far = down ? near - spacing : near + spacing;
            (int24 lower, int24 upper) = down ? (far, near) : (near, far);
            IUniswapV3PoolForSale(pool).mint(address(this), lower, upper, 1e17, "");
        }
        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](1);
        listings[0] = RobinhoodDriver.Listing(
            address(stock), IAggregatorV3(address(new MockFeed(1e8))), IUniswapV3Pool(pool), 1e30
        );
        RobinhoodDriver thin = new RobinhoodDriver(
            RobinhoodDriver.Venue({
                dollar: IERC20(USDC),
                router: ROUTER,
                safeSingleton: SAFE_SINGLETON,
                safeFactory: ISafeProxyFactory(SAFE_FACTORY),
                rolesMastercopy: ROLES_MASTERCOPY,
                moduleFactory: IModuleProxyFactory(MODULE_FACTORY),
                maxFeedAge: 1 hours,
                twapWindow: 30 minutes
            }),
            listings
        );
        StationDouble holder = new StationDouble(USDC, 500);
        holder.approveKey(IDriver(address(thin)), makeAddr("thin executor"));
        address safe = thin.safeOf(address(holder));
        // Far more than the whole pool holds within reach.
        stock.transfer(safe, 1e24);

        // Without a limit, one sale of it crosses all 300 ticks.
        {
            uint256 snap = vm.snapshotState();
            stock.approve(ROUTER, type(uint256).max);
            uint256 before = gasleft();
            ISwapRouter02(ROUTER).exactInputSingle(
                ISwapRouter02.ExactInputSingleParams({
                    tokenIn: address(stock),
                    tokenOut: USDC,
                    fee: 500,
                    recipient: address(this),
                    amountIn: 1e24,
                    amountOutMinimum: 0,
                    sqrtPriceLimitX96: 0
                })
            );
            uint256 unbounded = before - gasleft();
            console2.log("the same sale without a price limit (gas)", unbounded);
            assertGt(unbounded, StationCloses.ATTEMPT_GAS, "the pool is not griefed enough to show anything");
            vm.revertToState(snap);
        }

        int24 reach = int24(int256(thin.SALE_SPACINGS())) * spacing;
        for (uint256 retry = 0; retry < 3; retry++) {
            // A quiet half hour since the pool last moved: its time-weighted
            // price, which bounds the sale too, is where the last sale left it.
            vm.warp(vm.getBlockTimestamp() + 30 minutes + 1);
            (, int24 tickBefore,,,,,) = IUniswapV3Pool(pool).slot0();
            uint256 heldBefore = stock.balanceOf(safe);
            uint256 homeBefore = IERC20(USDC).balanceOf(address(holder));
            (Call[] memory calls, bool blocked) = thin.close(address(holder), 1e18, 10_000);
            assertTrue(blocked, "a close that sells said it was done");
            uint256 worst;
            for (uint256 i = 0; i < calls.length; i++) {
                (bool ok, uint256 spent) =
                    holder.attempt(IDriver(address(thin)), calls[i], StationCloses.ATTEMPT_GAS);
                assertTrue(ok, "a call of the sale failed within its allowance");
                if (spent > worst) worst = spent;
            }
            (, int24 tickAfter,,,,,) = IUniswapV3Pool(pool).slot0();
            console2.log("retry / the sale's gas", retry, worst);
            assertLt(
                worst, StationCloses.ATTEMPT_GAS * 3 / 4, "the bounded sale is not well within its allowance"
            );
            assertLt(stock.balanceOf(safe), heldBefore, "a retry sold nothing");
            assertGt(IERC20(USDC).balanceOf(address(holder)), homeBefore, "a retry brought nothing home");
            int24 moved = down ? tickBefore - tickAfter : tickAfter - tickBefore;
            assertGt(moved, reach - spacing, "the sale stopped short of its limit");
            // Landing on an initialised tick on the way down reads as the tick below it.
            assertLe(moved, reach + 1, "the sale went past its limit");
        }
    }

    function uniswapV3MintCallback(uint256 amount0, uint256 amount1, bytes calldata) external {
        (bool ok0, bytes memory d0) = msg.sender.staticcall(abi.encodeWithSignature("token0()"));
        (bool ok1, bytes memory d1) = msg.sender.staticcall(abi.encodeWithSignature("token1()"));
        require(ok0 && ok1);
        if (amount0 != 0) IERC20(abi.decode(d0, (address))).transfer(msg.sender, amount0);
        if (amount1 != 0) IERC20(abi.decode(d1, (address))).transfer(msg.sender, amount1);
    }
}
