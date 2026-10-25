// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {
    IAggregatorV3,
    IModuleProxyFactory,
    IRoles,
    ISafe,
    ISafeProxyFactory,
    IStockToken,
    ISwapRouter02,
    IUniswapV3Pool
} from "../../src/drivers/robinhood/RobinhoodChain.sol";
import {RobinhoodDriver} from "../../src/drivers/robinhood/RobinhoodDriver.sol";
import {Call, IDriver} from "../../src/interfaces/IDriver.sol";
import {StationDouble} from "./StationDouble.sol";
import {Test} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// The Robinhood driver against the real contracts on a fork of Robinhood
/// Chain mainnet: Safe v1.4.1, Zodiac Roles v2, Uniswap v3, Chainlink's
/// NVDA and TSLA feeds and the stock tokens themselves. Read-only: the fork
/// is local and nothing is sent to the chain.
///
/// Runs only with `ROBINHOOD_FORK_URL` set (a Robinhood Chain mainnet RPC,
/// e.g. `https://rpc.mainnet.chain.robinhood.com`); skipped otherwise, so the
/// suite never depends on a public node.
contract RobinhoodForkTest is Test {
    address internal constant USDG = 0x5fc5360D0400a0Fd4f2af552ADD042D716F1d168;
    address internal constant ROUTER = 0xCaf681a66D020601342297493863E78C959E5cb2;
    address internal constant NVDA = 0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC;
    address internal constant NVDA_POOL = 0xd4EB21209C4D6093f80B5b84f5C45cc093EA14a3;
    address internal constant NVDA_FEED = 0x379EC4f7C378F34a1B47E4F3cbeBCbAC3E8E9F15;
    address internal constant TSLA = 0x322F0929c4625eD5bAd873c95208D54E1c003b2d;
    address internal constant TSLA_POOL = 0xc4f0172D6ac8DD294Dd1137D047d5E1893760236;
    address internal constant TSLA_FEED = 0x4A1166a659A55625345e9515b32adECea5547C38;
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
        string memory url = vm.envOr("ROBINHOOD_FORK_URL", string(""));
        if (bytes(url).length == 0) return;
        vm.createSelectFork(url);
        forked = true;

        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](2);
        listings[0] = RobinhoodDriver.Listing(
            NVDA, IAggregatorV3(NVDA_FEED), IUniswapV3Pool(NVDA_POOL), type(uint256).max
        );
        listings[1] = RobinhoodDriver.Listing(
            TSLA, IAggregatorV3(TSLA_FEED), IUniswapV3Pool(TSLA_POOL), type(uint256).max
        );
        driver = new RobinhoodDriver(
            RobinhoodDriver.Venue({
                dollar: IERC20(USDG),
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
        station = new StationDouble(USDG, BAND);
        deal(USDG, address(station), 10_000e6);
    }

    modifier onFork() {
        if (!forked) {
            vm.skip(true);
            return;
        }
        _;
    }

    function _safe() internal view returns (address) {
        return driver.safeOf(address(station));
    }

    function _roles() internal view returns (IRoles) {
        return IRoles(driver.rolesOf(address(station)));
    }

    function _feed(address feed) internal view returns (uint256 price) {
        (, int256 answer,,,) = IAggregatorV3(feed).latestRoundData();
        price = uint256(answer);
    }

    /// What `amount` dollars buy of NVDA at its feed: 1e18 units a share, the
    /// feed in 1e8 dollars, the dollar in 1e6.
    function _nvdaFor(uint256 dollars) internal view returns (uint256) {
        return dollars * 1e20 / _feed(NVDA_FEED);
    }

    /// The executor, through its role: approve the router, then buy NVDA.
    function _buyNvda(uint256 dollars, uint256 minOut, address recipient) internal {
        vm.startPrank(executor);
        _roles().execTransactionWithRole(
            USDG, 0, abi.encodeCall(IERC20.approve, (ROUTER, dollars)), 0, ROLE, true
        );
        _roles().execTransactionWithRole(
            ROUTER,
            0,
            abi.encodeCall(
                ISwapRouter02.exactInputSingle,
                (
                    ISwapRouter02.ExactInputSingleParams({
                        tokenIn: USDG,
                        tokenOut: NVDA,
                        fee: 500,
                        recipient: recipient,
                        amountIn: dollars,
                        amountOutMinimum: minOut,
                        sqrtPriceLimitX96: 0
                    })
                )
            ),
            0,
            ROLE,
            true
        );
        vm.stopPrank();
    }

    function _setUpAndFund() internal {
        station.approveKey(IDriver(address(driver)), executor);
        station.deposit(IDriver(address(driver)), 5000e6);
    }

    // The account -----------------------------------------------------------

    function test_approving_a_key_makes_a_safe_owned_only_by_the_station_with_the_role_module_enabled()
        public
        onFork
    {
        assertEq(_safe().code.length, 0);
        station.approveKey(IDriver(address(driver)), executor);
        ISafe safe = ISafe(_safe());
        address[] memory owners = safe.getOwners();
        assertEq(owners.length, 1);
        assertEq(owners[0], address(station), "the station is the Safe's only owner");
        assertEq(safe.getThreshold(), 1);
        assertTrue(safe.isModuleEnabled(address(_roles())), "the role module acts for the Safe");
        assertEq(_roles().owner(), address(station), "the station owns the role module");
        assertEq(_roles().avatar(), address(safe));
        assertTrue(driver.isSetUp(address(station)));
    }

    function test_a_second_approval_scopes_nothing_again_and_only_adds_the_member() public onFork {
        station.approveKey(IDriver(address(driver)), executor);
        address other = makeAddr("other executor");
        assertEq(driver.approveKey(address(station), other, 0).length, 1, "only the membership");
    }

    function test_a_deposit_moves_dollars_from_the_station_into_its_safe() public onFork {
        _setUpAndFund();
        assertEq(IERC20(USDG).balanceOf(_safe()), 5000e6);
        assertEq(IERC20(USDG).balanceOf(address(station)), 5000e6);
        station.withdraw(IDriver(address(driver)), 1000e6);
        assertEq(IERC20(USDG).balanceOf(_safe()), 4000e6);
        assertEq(IERC20(USDG).balanceOf(address(station)), 6000e6);
    }

    // What the executor's role allows ---------------------------------------

    function test_the_executor_buys_a_listed_stock_paid_to_the_safe_within_the_band_of_its_feed()
        public
        onFork
    {
        _setUpAndFund();
        uint256 fair = _nvdaFor(1000e6);
        _buyNvda(1000e6, fair * 98 / 100, _safe());
        assertGt(IERC20(NVDA).balanceOf(_safe()), fair * 98 / 100, "the Safe holds the NVDA it bought");
        assertEq(IERC20(USDG).balanceOf(_safe()), 4000e6);
    }

    function test_the_executor_cannot_pay_a_swap_to_anyone_but_the_safe() public onFork {
        _setUpAndFund();
        uint256 fair = _nvdaFor(1000e6);
        vm.expectRevert();
        this.buyAs(1000e6, fair * 98 / 100, executor);
    }

    function test_the_executor_cannot_swap_for_less_than_the_feed_allows_within_the_band() public onFork {
        _setUpAndFund();
        uint256 fair = _nvdaFor(1000e6);
        address safe = _safe();
        vm.expectRevert();
        this.buyAs(1000e6, fair * 94 / 100, safe);
        vm.expectRevert();
        this.buyAs(1000e6, 0, safe);
    }

    function test_the_executor_cannot_move_dollars_out_of_the_safe() public onFork {
        _setUpAndFund();
        IRoles roles = _roles();
        vm.startPrank(executor);
        vm.expectRevert();
        roles.execTransactionWithRole(
            USDG, 0, abi.encodeCall(IERC20.transfer, (executor, 1e6)), 0, ROLE, true
        );
        vm.expectRevert();
        roles.execTransactionWithRole(USDG, 0, abi.encodeCall(IERC20.approve, (executor, 1e6)), 0, ROLE, true);
        vm.stopPrank();
    }

    function test_a_revoked_key_can_no_longer_trade() public onFork {
        _setUpAndFund();
        station.revokeKey(IDriver(address(driver)), executor);
        uint256 fair = _nvdaFor(1000e6);
        address safe = _safe();
        vm.expectRevert();
        this.buyAs(1000e6, fair * 98 / 100, safe);
    }

    function buyAs(uint256 dollars, uint256 minOut, address recipient) external {
        _buyNvda(dollars, minOut, recipient);
    }

    // Valuing and closing ---------------------------------------------------

    function test_holdings_are_valued_at_the_feed_without_the_multiplier_and_at_the_pools_twap_on_the_exit_side(
    ) public onFork {
        _setUpAndFund();
        _buyNvda(2000e6, _nvdaFor(2000e6) * 98 / 100, _safe());
        uint256 held = IERC20(NVDA).balanceOf(_safe());
        (uint256 mark, uint256 exit) = driver.valueOf(address(station));
        // Balance times the feed: the feed already carries the multiplier.
        assertEq(mark, 3000e6 + held * _feed(NVDA_FEED) / 1e20);
        // The exit side is the pool's time-weighted price: near the feed.
        assertApproxEqRel(exit, mark, 0.05e18);
    }

    function test_a_leavers_close_sells_their_fraction_and_pays_the_station_what_it_fetched() public onFork {
        _setUpAndFund();
        _buyNvda(2000e6, _nvdaFor(2000e6) * 98 / 100, _safe());
        uint256 held = IERC20(NVDA).balanceOf(_safe());
        uint256 before = IERC20(USDG).balanceOf(address(station));
        (uint256 markBefore,) = driver.valueOf(address(station));

        assertFalse(station.close(IDriver(address(driver)), 0.25e18));

        assertEq(IERC20(NVDA).balanceOf(_safe()), held - (held + 3) / 4, "a quarter of the NVDA sold");
        assertEq(IERC20(USDG).balanceOf(_safe()), 2250e6, "a quarter of the idle dollars went too");
        uint256 got = IERC20(USDG).balanceOf(address(station)) - before;
        assertApproxEqRel(got, markBefore / 4, 0.02e18, "the station got about a quarter of the worth");
        assertGe(got, 750e6 + (held / 4) * _feed(NVDA_FEED) / 1e20 * (10_000 - BAND) / 10_000);
    }

    function test_a_feed_older_than_the_limit_blocks_the_close() public onFork {
        _setUpAndFund();
        _buyNvda(2000e6, _nvdaFor(2000e6) * 98 / 100, _safe());
        (uint80 round, int256 answer,,,) = IAggregatorV3(NVDA_FEED).latestRoundData();
        vm.mockCall(
            NVDA_FEED,
            abi.encodeCall(IAggregatorV3.latestRoundData, ()),
            abi.encode(round, answer, 0, block.timestamp - 4 days - 1, round)
        );
        (, bool blocked) = driver.close(address(station), 1e18, BAND);
        assertTrue(blocked);
    }

    function test_a_feed_hours_old_alone_blocks_nothing() public onFork {
        _setUpAndFund();
        _buyNvda(2000e6, _nvdaFor(2000e6) * 98 / 100, _safe());
        (uint80 round, int256 answer,,,) = IAggregatorV3(NVDA_FEED).latestRoundData();
        vm.mockCall(
            NVDA_FEED,
            abi.encodeCall(IAggregatorV3.latestRoundData, ()),
            abi.encode(round, answer, 0, block.timestamp - 60 hours, round)
        );
        (, bool blocked) = driver.close(address(station), 1e18, BAND);
        assertFalse(blocked, "a weekend-old feed that agrees with the market is used");
    }

    function test_a_paused_token_oracle_blocks_the_close() public onFork {
        _setUpAndFund();
        _buyNvda(2000e6, _nvdaFor(2000e6) * 98 / 100, _safe());
        vm.mockCall(NVDA, abi.encodeCall(IStockToken.oraclePaused, ()), abi.encode(true));
        (, bool blocked) = driver.close(address(station), 1e18, BAND);
        assertTrue(blocked);
    }

    function test_a_market_further_from_the_feed_than_the_band_blocks_the_close_on_either_side()
        public
        onFork
    {
        _setUpAndFund();
        _buyNvda(2000e6, _nvdaFor(2000e6) * 98 / 100, _safe());
        (uint80 round, int256 answer,, uint256 updatedAt,) = IAggregatorV3(NVDA_FEED).latestRoundData();
        vm.mockCall(
            NVDA_FEED,
            abi.encodeCall(IAggregatorV3.latestRoundData, ()),
            abi.encode(round, answer * 110 / 100, 0, updatedAt, round)
        );
        (, bool blocked) = driver.close(address(station), 1e18, BAND);
        assertTrue(blocked, "feed above the market");
        vm.mockCall(
            NVDA_FEED,
            abi.encodeCall(IAggregatorV3.latestRoundData, ()),
            abi.encode(round, answer * 90 / 100, 0, updatedAt, round)
        );
        (, blocked) = driver.close(address(station), 1e18, BAND);
        assertTrue(blocked, "feed below the market");
    }

    function test_at_the_full_band_everything_is_sold_at_any_price_whatever_the_feed() public onFork {
        _setUpAndFund();
        _buyNvda(2000e6, _nvdaFor(2000e6) * 98 / 100, _safe());
        vm.mockCall(NVDA, abi.encodeCall(IStockToken.oraclePaused, ()), abi.encode(true));
        (Call[] memory offered, bool blocked) = driver.close(address(station), 1e18, 10_000);
        assertGt(offered.length, 0, "a full-band close made no sale");
        // Not done until a close finds nothing left to sell.
        assertTrue(blocked, "a full-band close that sells said it was done");
        uint256 before = IERC20(USDG).balanceOf(address(station));
        (Call[] memory calls,) = driver.close(address(station), 1e18, 10_000);
        station.run(IDriver(address(driver)), calls);
        assertEq(IERC20(NVDA).balanceOf(_safe()), 0);
        assertEq(IERC20(USDG).balanceOf(_safe()), 0);
        assertGt(IERC20(USDG).balanceOf(address(station)), before + 4800e6);
    }

    function test_a_dust_donation_of_a_token_with_a_broken_feed_holds_nothing_off() public onFork {
        _setUpAndFund();
        deal(TSLA, _safe(), 1e12);
        vm.mockCall(TSLA, abi.encodeCall(IStockToken.oraclePaused, ()), abi.encode(true));
        (, bool blocked) = driver.close(address(station), 0.5e18, BAND);
        assertFalse(blocked);
    }
}
