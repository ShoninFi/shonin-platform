// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {
    IAggregatorV3,
    IModuleProxyFactory,
    IRoles,
    ISafeProxyFactory,
    IStockToken,
    ISwapRouter02,
    IUniswapV3Pool
} from "../../src/drivers/robinhood/RobinhoodChain.sol";
import {RobinhoodDriver} from "../../src/drivers/robinhood/RobinhoodDriver.sol";
import {IDriver} from "../../src/interfaces/IDriver.sol";
import {StationDouble} from "./StationDouble.sol";
import {Test} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// The Robinhood driver listing WETH against Circle's USDC on a fork of Base
/// Sepolia, with the chain's own Safe v1.4.1, Zodiac Roles v2, Uniswap v3
/// and Chainlink ETH/USD feed. WETH has no `oraclePaused`: its fallback
/// takes the call as a deposit, which a view call can't make, so the
/// driver's check finds nothing paused and prices WETH from its feed.
///
/// Runs only with `BASE_SEPOLIA_FORK_URL` set (a Base Sepolia RPC, e.g.
/// `https://sepolia.base.org`); skipped otherwise.
contract BaseSepoliaForkTest is Test {
    address internal constant USDC = 0x036CbD53842c5426634e7929541eC2318f3dCF7e;
    address internal constant WETH = 0x4200000000000000000000000000000000000006;
    address internal constant ETH_FEED = 0x4aDC67696bA383F43DD60A9e78F2C97Fbbfc7cb1;
    address internal constant ROUTER = 0x94cC0AaC535CCDB3C01d6787D6413C739ae12bc4;
    address internal constant WETH_POOL = 0x94bfc0574FF48E92cE43d495376C477B1d0EEeC0;
    address internal constant SAFE_SINGLETON = 0x29fcB43b46531BcA003ddC8FCB67FFE91900C762;
    address internal constant SAFE_FACTORY = 0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67;
    address internal constant ROLES_MASTERCOPY = 0x9646fDAD06d3e24444381f44362a3B0eB343D337;
    address internal constant MODULE_FACTORY = 0x000000000000aDdB49795b0f9bA5BC298cDda236;

    uint16 internal constant BAND = 500;
    bytes32 internal constant ROLE = "shonin-executor";

    RobinhoodDriver internal driver;
    StationDouble internal station;
    address internal executor = makeAddr("executor");
    bool internal forked;

    function setUp() public {
        string memory url = vm.envOr("BASE_SEPOLIA_FORK_URL", string(""));
        if (bytes(url).length == 0) return;
        vm.createSelectFork(url);
        forked = true;

        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](1);
        listings[0] = RobinhoodDriver.Listing(WETH, IAggregatorV3(ETH_FEED), IUniswapV3Pool(WETH_POOL));
        driver = new RobinhoodDriver(
            RobinhoodDriver.Venue({
                dollar: IERC20(USDC),
                router: ROUTER,
                safeSingleton: SAFE_SINGLETON,
                safeFactory: ISafeProxyFactory(SAFE_FACTORY),
                rolesMastercopy: ROLES_MASTERCOPY,
                moduleFactory: IModuleProxyFactory(MODULE_FACTORY),
                maxFeedAge: 4 days,
                twapWindow: 30 minutes
            }),
            listings
        );
        station = new StationDouble(USDC, BAND);
    }

    modifier onFork() {
        if (!forked) {
            vm.skip(true);
            return;
        }
        _;
    }

    function _price() internal view returns (uint256) {
        (, int256 answer,,,) = IAggregatorV3(ETH_FEED).latestRoundData();
        return uint256(answer);
    }

    /// What `amount` of WETH is worth at the feed: 1e18 units a coin, the
    /// feed in 1e8 dollars, USDC in 1e6.
    function _worth(uint256 amount) internal view returns (uint256) {
        return amount * _price() / 1e20;
    }

    function _sale(uint256 amount, uint256 minOut, address recipient) internal pure returns (bytes memory) {
        return abi.encodeCall(
            ISwapRouter02.exactInputSingle,
            (
                ISwapRouter02.ExactInputSingleParams({
                    tokenIn: WETH,
                    tokenOut: USDC,
                    fee: 500,
                    recipient: recipient,
                    amountIn: amount,
                    amountOutMinimum: minOut,
                    sqrtPriceLimitX96: 0
                })
            )
        );
    }

    function _check(bytes memory call) internal view returns (bool ok, bytes32 reason) {
        // The swap's parameters start after the selector.
        return driver.check(ROUTER, 0, call, 0, 4, 0, bytes12(uint96(BAND)));
    }

    function test_weths_missing_oracle_pause_reverts_when_read_as_a_view() public onFork {
        vm.expectRevert();
        this.readPause();
    }

    function readPause() external view returns (bool) {
        return IStockToken(WETH).oraclePaused();
    }

    function test_judging_a_weth_sale_spends_a_bounded_amount_of_gas_whatever_it_is_given() public onFork {
        bytes memory sale = _sale(1e17, _worth(1e17), address(this));
        uint256 before = gasleft();
        (bool ok,) = _check(sale);
        uint256 spent = before - gasleft();
        assertTrue(ok);
        assertLt(spent, 150_000, "the failed pause read burns only its own allowance");
    }

    function test_weth_is_listed_with_the_feeds_scale_and_the_official_pool() public onFork {
        RobinhoodDriver.Stock[] memory stocks = driver.stocks();
        assertEq(stocks.length, 1);
        assertEq(stocks[0].token, WETH);
        assertEq(address(stocks[0].pool), WETH_POOL);
        assertEq(stocks[0].fee, 500);
        assertEq(stocks[0].scale, 10 ** (18 + 8 - 6));
    }

    function test_a_weth_sale_is_judged_at_the_chainlink_price_within_the_band() public onFork {
        uint256 fair = _worth(1e17);
        (bool ok,) = _check(_sale(1e17, fair * 96 / 100, address(this)));
        assertTrue(ok, "within the band of a fresh feed, nothing paused");
        bytes32 reason;
        (ok, reason) = _check(_sale(1e17, fair * 94 / 100, address(this)));
        assertFalse(ok);
        assertEq(reason, "price");
    }

    function test_weth_in_the_safe_is_marked_at_the_feed() public onFork {
        station.approveKey(IDriver(address(driver)), executor);
        address safe = driver.safeOf(address(station));
        deal(WETH, safe, 1e17);
        (uint256 mark,) = driver.valueOf(address(station));
        assertEq(mark, IERC20(USDC).balanceOf(safe) + 1e17 * _price() / 1e20);
    }

    function test_the_executor_sells_weth_from_the_safe_through_its_role_on_the_real_pool() public onFork {
        station.approveKey(IDriver(address(driver)), executor);
        address safe = driver.safeOf(address(station));
        deal(WETH, safe, 1e16);
        IRoles roles = IRoles(driver.rolesOf(address(station)));
        uint256 amount = 1e15;
        // Whatever the pool fetches, the role allows no less than the band
        // below the feed: ask for exactly that.
        uint256 minOut = _worth(amount) * (10_000 - BAND) / 10_000 + 1;
        (bool ok,) = _check(_sale(amount, minOut, safe));
        assertTrue(ok);
        vm.startPrank(executor);
        roles.execTransactionWithRole(
            WETH, 0, abi.encodeCall(IERC20.approve, (ROUTER, amount)), 0, ROLE, true
        );
        // The pool may not fetch the minimum; then the router, not the role,
        // refuses it.
        try roles.execTransactionWithRole(ROUTER, 0, _sale(amount, minOut, safe), 0, ROLE, true) {
            assertEq(IERC20(WETH).balanceOf(safe), 1e16 - amount, "the WETH left the Safe");
            assertGe(IERC20(USDC).balanceOf(safe), minOut, "and the USDC came back to it");
        } catch (bytes memory reason) {
            assertEq(
                keccak256(reason), keccak256(abi.encodeWithSignature("Error(string)", "Too little received"))
            );
        }
        vm.stopPrank();
    }

    function test_the_executor_cannot_sell_weth_below_the_band_of_the_feed() public onFork {
        station.approveKey(IDriver(address(driver)), executor);
        address safe = driver.safeOf(address(station));
        deal(WETH, safe, 1e16);
        IRoles roles = IRoles(driver.rolesOf(address(station)));
        vm.startPrank(executor);
        roles.execTransactionWithRole(WETH, 0, abi.encodeCall(IERC20.approve, (ROUTER, 1e15)), 0, ROLE, true);
        vm.expectRevert();
        roles.execTransactionWithRole(ROUTER, 0, _sale(1e15, 0, safe), 0, ROLE, true);
        vm.stopPrank();
    }
}
