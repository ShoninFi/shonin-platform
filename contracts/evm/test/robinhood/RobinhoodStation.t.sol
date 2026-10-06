// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {Vault} from "../../src/Vault.sol";

import {
    IAggregatorV3,
    IModuleProxyFactory,
    IRoles,
    ISafeProxyFactory,
    ISwapRouter02,
    IUniswapV3Pool
} from "../../src/drivers/robinhood/RobinhoodChain.sol";
import {RobinhoodDriver} from "../../src/drivers/robinhood/RobinhoodDriver.sol";
import {ChainNeed} from "../../src/interfaces/IStrategyRegistry.sol";
import {ClaimState} from "../../src/interfaces/IVault.sol";
import {Fixture} from "../Fixture.sol";
import {IUniswapV3FactoryLite, PoolSeeder} from "./PoolSeeder.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A whole vault whose station holds NVDA through the Robinhood driver, on a
/// fork of Robinhood Chain mainnet: the platform's own contracts as the
/// development chain deploys them, the real Safe, Roles v2, Uniswap v3,
/// NVDA token and NVDA feed. The fixture's dollar has no pool on mainnet, so
/// a pool of NVDA against it is made on the fork at the feed's price.
///
/// Runs only with `ROBINHOOD_FORK_URL` set; skipped otherwise.
contract RobinhoodStationTest is Fixture {
    address internal constant ROUTER = 0xCaf681a66D020601342297493863E78C959E5cb2;
    address internal constant UNISWAP_FACTORY = 0x1f7d7550B1b028f7571E69A784071F0205FD2EfA;
    address internal constant NVDA = 0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC;
    address internal constant NVDA_FEED = 0x379EC4f7C378F34a1B47E4F3cbeBCbAC3E8E9F15;
    address internal constant SAFE_SINGLETON = 0x29fcB43b46531BcA003ddC8FCB67FFE91900C762;
    address internal constant SAFE_FACTORY = 0x4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67;
    address internal constant ROLES_MASTERCOPY = 0x9646fDAD06d3e24444381f44362a3B0eB343D337;
    address internal constant MODULE_FACTORY = 0x000000000000aDdB49795b0f9bA5BC298cDda236;
    bytes32 internal constant RH = "robinhood";
    bytes32 internal constant ROLE = "shonin-executor";

    bool internal forked;
    RobinhoodDriver internal rh;
    IUniswapV3Pool internal pool;
    Vault internal vault;
    Station internal station;

    function setUp() public override {
        string memory url = vm.envOr("ROBINHOOD_FORK_URL", string(""));
        if (bytes(url).length == 0) return;
        vm.createSelectFork(url);
        forked = true;
        uint256 now_ = block.timestamp;
        super.setUp();
        // The fixture starts its clock early; the feeds live in the present.
        vm.warp(now_ + 1);

        PoolSeeder seeder = new PoolSeeder();
        deal(NVDA, address(seeder), 100_000e18);
        usd.mint(address(seeder), 30_000_000e6);
        (, int256 answer,,,) = IAggregatorV3(NVDA_FEED).latestRoundData();
        pool = IUniswapV3Pool(
            seeder.seed(
                IUniswapV3FactoryLite(UNISWAP_FACTORY), NVDA, address(usd), 500, uint256(answer), 8, 6, 1e18
            )
        );
        // Let the pool's time-weighted price cover its window.
        skip(31 minutes);
        _keepFeedFresh();

        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](1);
        listings[0] = RobinhoodDriver.Listing(NVDA, IAggregatorV3(NVDA_FEED), pool);
        rh = new RobinhoodDriver(
            RobinhoodDriver.Venue({
                dollar: IERC20(address(usd)),
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
        vm.prank(admin);
        attestation.proposeDriver(RH, address(rh));
        skip(TIMELOCK);
        _keepFeedFresh();

        uint256 id = submitAs(author);
        bytes32 bundle = keccak256(abi.encode("bundle", id));
        _register(id, bundle);
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(bundle, 1));
        vault = activate(id, allowancesCosting(2e6), MINIMUM_TANK);
        blessExecutorKey(executor, address(vault), bundle);
        station = stationOf(vault);
    }

    /// The feed's latest round, kept as it is but dated now: the fork's clock
    /// moves on through the fixture's timelocks and the feed doesn't.
    function _keepFeedFresh() internal {
        vm.clearMockedCalls();
        (uint80 round, int256 answer,,,) = IAggregatorV3(NVDA_FEED).latestRoundData();
        vm.mockCall(
            NVDA_FEED,
            abi.encodeCall(IAggregatorV3.latestRoundData, ()),
            abi.encode(round, answer, block.timestamp, block.timestamp, round)
        );
    }

    modifier onFork() {
        if (!forked) {
            vm.skip(true);
            return;
        }
        _;
    }

    function _register(uint256 id, bytes32 bundleHash) internal {
        ChainNeed[] memory chains = new ChainNeed[](1);
        chains[0].chain = "base";
        chains[0].drivers = new bytes32[](1);
        chains[0].drivers[0] = RH;
        bytes32[] memory none = new bytes32[](0);
        bytes32[] memory restricted = new bytes32[](2);
        restricted[0] = "SANCTIONED";
        restricted[1] = "US";
        bytes32 digest = strategies.registrationDigest(
            id, author, keccak256("source"), bundleHash, keccak256("manifest"), chains, none, restricted
        );
        strategies.register(
            id,
            author,
            keccak256("source"),
            bundleHash,
            keccak256("manifest"),
            chains,
            none,
            restricted,
            signed(BUILDER_KEY, digest),
            signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")))
        );
    }

    /// Alice and Bob each put in 10k; the executor moves 15k into the Safe
    /// and buys 10k of NVDA through its role.
    function _fundAndTrade() internal returns (uint256 aliceShares) {
        aliceShares = depositAs(vault, alice, 10_000e6);
        depositAs(vault, bob, 10_000e6);
        _keepFeedFresh();
        vm.startPrank(executor);
        station.pull(15_000e6);
        station.approveKey(RH, executor);
        station.deposit(RH, 15_000e6);
        IRoles roles = IRoles(rh.rolesOf(address(station)));
        address safe = rh.safeOf(address(station));
        (, int256 answer,,,) = IAggregatorV3(NVDA_FEED).latestRoundData();
        uint256 fair = uint256(10_000e6) * 1e20 / uint256(answer);
        roles.execTransactionWithRole(
            address(usd), 0, abi.encodeCall(IERC20.approve, (ROUTER, 10_000e6)), 0, ROLE, true
        );
        roles.execTransactionWithRole(
            ROUTER,
            0,
            abi.encodeCall(
                ISwapRouter02.exactInputSingle,
                (
                    ISwapRouter02.ExactInputSingleParams({
                        tokenIn: address(usd),
                        tokenOut: NVDA,
                        fee: 500,
                        recipient: safe,
                        amountIn: 10_000e6,
                        amountOutMinimum: fair * 995 / 1000,
                        sqrtPriceLimitX96: 0
                    })
                )
            ),
            0,
            ROLE,
            true
        );
        vm.stopPrank();
        // Let the pool's average catch up with the trade.
        skip(31 minutes);
        _keepFeedFresh();
    }

    function test_the_station_is_valued_with_its_safe_at_the_feed_and_the_pool() public onFork {
        _fundAndTrade();
        (uint256 mark, uint256 exit) = station.value();
        // The 15k it was given, 10k of it now NVDA, give or take the swap.
        assertApproxEqRel(mark, 15_000e6, 0.01e18);
        assertApproxEqRel(exit, mark, 0.01e18);
        assertFalse(station.outsideBand());
    }

    function test_a_claim_sells_the_leavers_fraction_from_the_safe_and_pays_them_in_the_same_transaction()
        public
        onFork
    {
        uint256 aliceShares = _fundAndTrade();
        address safe = rh.safeOf(address(station));
        uint256 nvdaBefore = IERC20(NVDA).balanceOf(safe);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        _keepFeedFresh();
        assertEq(vault.claimCount(), 1, "the cash couldn't pay it, so it is a claim");
        uint256 before = usd.balanceOf(alice);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "paid at its close");
        uint256 paid = usd.balanceOf(alice) - before;
        assertApproxEqRel(paid, 10_000e6, 0.01e18, "about her half");
        assertApproxEqRel(IERC20(NVDA).balanceOf(safe), nvdaBefore / 2, 0.01e18, "half the NVDA was sold");
    }

    function test_a_feed_far_from_the_pool_voids_the_valuation_and_holds_the_claim() public onFork {
        uint256 aliceShares = _fundAndTrade();
        (uint80 round, int256 answer,,,) = IAggregatorV3(NVDA_FEED).latestRoundData();
        vm.mockCall(
            NVDA_FEED,
            abi.encodeCall(IAggregatorV3.latestRoundData, ()),
            abi.encode(round, answer * 120 / 100, block.timestamp, block.timestamp, round)
        );
        assertTrue(station.outsideBand());
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        uint256 before = usd.balanceOf(alice);
        value(vault);
        assertEq(usd.balanceOf(alice), before, "nothing is paid at a price nobody trusts");
    }
}
