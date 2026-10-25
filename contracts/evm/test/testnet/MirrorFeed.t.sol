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
import {AggregatorV3Interface, IMirrorFeed} from "./IMirrorFeed.sol";
import {MirrorFeed} from "./MirrorFeed.sol";
import {Test} from "forge-std/Test.sol";

import {ERC20} from "openzeppelin/token/ERC20/ERC20.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

contract Token is ERC20 {
    uint8 private immutable _decimals;

    constructor(string memory symbol, uint8 decimals_) ERC20(symbol, symbol) {
        _decimals = decimals_;
    }

    function decimals() public view override returns (uint8) {
        return _decimals;
    }
}

/// What the driver's constructor asks of a pool: its pair and fee.
contract PoolDouble {
    address public immutable token0;
    address public immutable token1;
    uint24 public constant fee = 500;

    constructor(address a, address b) {
        (token0, token1) = a < b ? (a, b) : (b, a);
    }
}

/// What the driver's constructor asks of the Safe proxy factory.
contract SafeFactoryDouble {
    function proxyCreationCode() external pure returns (bytes memory) {
        return hex"6080";
    }
}

contract MirrorFeedTest is Test {
    /// TSLA's feed on Robinhood Chain mainnet, and a round it published.
    address internal constant SOURCE = 0x4A1166a659A55625345e9515b32adECea5547C38;
    uint256 internal constant SOURCE_CHAIN = 4663;
    uint80 internal constant ROUND = 18_446_744_073_709_553_211;
    int256 internal constant ANSWER = 36_782_625_000;
    uint256 internal constant UPDATED = 1_792_423_326;

    address internal updater = makeAddr("updater");
    MirrorFeed internal feed;

    function setUp() public {
        vm.warp(UPDATED + 60);
        feed = new MirrorFeed(updater, 8, "RHTSLA / USD", 6, SOURCE, SOURCE_CHAIN, ROUND, ANSWER, UPDATED);
    }

    function test_it_answers_as_a_chainlink_feed_with_the_source_s_own_round() public view {
        AggregatorV3Interface chainlink = AggregatorV3Interface(address(feed));
        assertEq(chainlink.decimals(), 8);
        assertEq(chainlink.description(), "RHTSLA / USD");
        assertEq(chainlink.version(), 6);
        (uint80 round, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredIn) =
            chainlink.latestRoundData();
        assertEq(round, ROUND);
        assertEq(answer, ANSWER);
        assertEq(startedAt, UPDATED);
        assertEq(updatedAt, UPDATED);
        assertEq(answeredIn, ROUND);
        assertEq(feed.source(), SOURCE);
        assertEq(feed.sourceChainId(), SOURCE_CHAIN);
    }

    function test_a_newer_round_becomes_the_latest_and_the_older_one_stays_readable() public {
        vm.prank(updater);
        feed.update(ROUND + 1, ANSWER + 1e8, UPDATED + 600);
        (uint80 round, int256 answer,, uint256 updatedAt,) = feed.latestRoundData();
        assertEq(round, ROUND + 1);
        assertEq(answer, ANSWER + 1e8);
        assertEq(updatedAt, UPDATED + 600);
        (round, answer,, updatedAt,) = feed.getRoundData(ROUND);
        assertEq(round, ROUND);
        assertEq(answer, ANSWER);
        assertEq(updatedAt, UPDATED);
    }

    function test_a_round_ids_gap_is_kept_as_the_source_numbered_it() public {
        // The source's phases jump its ids; the copy keeps them as they are.
        uint80 nextPhase = (uint80(2) << 64) | 1;
        vm.prank(updater);
        feed.update(nextPhase, ANSWER, UPDATED + 1);
        (uint80 round,,,,) = feed.latestRoundData();
        assertEq(round, nextPhase);
    }

    function test_only_the_updater_posts_and_it_may_hand_posting_over() public {
        vm.expectRevert(IMirrorFeed.NotUpdater.selector);
        feed.update(ROUND + 1, ANSWER, UPDATED + 1);
        vm.expectRevert(IMirrorFeed.NotUpdater.selector);
        feed.setUpdater(address(this));

        address next = makeAddr("next");
        vm.prank(updater);
        feed.setUpdater(next);
        assertEq(feed.updater(), next);
        vm.prank(updater);
        vm.expectRevert(IMirrorFeed.NotUpdater.selector);
        feed.update(ROUND + 1, ANSWER, UPDATED + 1);
        vm.prank(next);
        feed.update(ROUND + 1, ANSWER, UPDATED + 1);
    }

    function test_an_older_or_repeated_round_or_one_updated_earlier_is_refused() public {
        vm.startPrank(updater);
        vm.expectRevert(IMirrorFeed.StaleRound.selector);
        feed.update(ROUND, ANSWER, UPDATED + 1);
        vm.expectRevert(IMirrorFeed.StaleRound.selector);
        feed.update(ROUND - 1, ANSWER, UPDATED + 1);
        vm.expectRevert(IMirrorFeed.StaleRound.selector);
        feed.update(ROUND + 1, ANSWER, UPDATED - 1);
        vm.expectRevert(IMirrorFeed.StaleRound.selector);
        feed.update(ROUND + 1, ANSWER, 0);
        // The same moment as the latest is not older: a source round can be
        // followed by another within the same second.
        feed.update(ROUND + 1, ANSWER, UPDATED);
        vm.stopPrank();
    }

    function test_a_round_never_posted_is_not_answered() public {
        vm.expectRevert(IMirrorFeed.NoDataPresent.selector);
        feed.getRoundData(ROUND - 1);
        vm.expectRevert(IMirrorFeed.StaleRound.selector);
        new MirrorFeed(updater, 8, "", 6, SOURCE, SOURCE_CHAIN, 0, ANSWER, UPDATED);
    }

    function testFuzz_the_latest_round_only_moves_forward(uint80 a, uint80 b, uint32 ta, uint32 tb) public {
        vm.assume(a > ROUND && b > ROUND);
        vm.startPrank(updater);
        bool first = _tryUpdate(a, uint256(UPDATED) + ta);
        bool second = _tryUpdate(b, uint256(UPDATED) + tb);
        vm.stopPrank();
        assertTrue(first, "a newer round after the constructor's is taken");
        assertEq(second, b > a && tb >= ta);
        (uint80 latest,,, uint256 updatedAt,) = feed.latestRoundData();
        assertEq(latest, second ? b : a);
        assertEq(updatedAt, uint256(UPDATED) + (second ? tb : ta));
    }

    function _tryUpdate(uint80 round, uint256 updatedAt) internal returns (bool) {
        try feed.update(round, ANSWER, updatedAt) {
            return true;
        } catch {
            return false;
        }
    }
}

/// The Robinhood driver reads a mirror feed exactly as it reads Chainlink's
/// own: the role's swap check prices from it and voids it when it is old.
contract DriverReadsAMirrorFeedTest is Test {
    uint16 internal constant BAND = 500;
    address internal router = makeAddr("router");
    address internal updater = makeAddr("updater");
    Token internal dollar;
    Token internal stock;
    MirrorFeed internal feed;
    RobinhoodDriver internal driver;

    function setUp() public {
        vm.warp(2_000_000_000);
        dollar = new Token("USDG", 6);
        stock = new Token("TSLA", 18);
        feed =
            new MirrorFeed(updater, 8, "RHTSLA / USD", 6, address(1), 4663, 7, 400e8, block.timestamp - 3600);
        RobinhoodDriver.Listing[] memory listings = new RobinhoodDriver.Listing[](1);
        listings[0] = RobinhoodDriver.Listing(
            address(stock),
            IAggregatorV3(address(feed)),
            IUniswapV3Pool(address(new PoolDouble(address(stock), address(dollar)))),
            type(uint256).max
        );
        driver = new RobinhoodDriver(
            RobinhoodDriver.Venue({
                dollar: IERC20(address(dollar)),
                router: router,
                safeSingleton: address(0x5afe),
                safeFactory: ISafeProxyFactory(address(new SafeFactoryDouble())),
                rolesMastercopy: address(0x4011),
                moduleFactory: IModuleProxyFactory(address(0x1111)),
                maxFeedAge: 4 days,
                twapWindow: 30 minutes
            }),
            listings
        );
    }

    function _sale(uint256 tokens, uint256 minDollars) internal view returns (bytes memory) {
        return abi.encode(
            ISwapRouter02.ExactInputSingleParams({
                tokenIn: address(stock),
                tokenOut: address(dollar),
                fee: 500,
                recipient: address(this),
                amountIn: tokens,
                amountOutMinimum: minDollars,
                sqrtPriceLimitX96: 0
            })
        );
    }

    function _check(bytes memory params) internal view returns (bool ok, bytes32 reason) {
        return driver.check(router, 0, params, 0, 0, 0, bytes12(uint96(BAND)));
    }

    function test_the_driver_lists_the_mirror_at_the_feed_s_decimals() public view {
        RobinhoodDriver.Stock[] memory stocks = driver.stocks();
        assertEq(address(stocks[0].feed), address(feed));
        assertEq(stocks[0].scale, 10 ** (18 + 8 - 6));
    }

    function test_a_sale_is_judged_at_the_mirror_s_price_within_the_band() public view {
        // One share at $400: the band allows $380.
        (bool ok,) = _check(_sale(1e18, 380e6));
        assertTrue(ok);
        bytes32 reason;
        (ok, reason) = _check(_sale(1e18, 379e6));
        assertFalse(ok);
        assertEq(reason, "price");
    }

    function test_a_new_round_moves_the_price_the_driver_judges_by() public {
        vm.prank(updater);
        feed.update(8, 500e8, block.timestamp);
        // What was within the band at $400 is now too cheap at $500.
        (bool ok, bytes32 reason) = _check(_sale(1e18, 380e6));
        assertFalse(ok);
        assertEq(reason, "price");
        (ok,) = _check(_sale(1e18, 476e6));
        assertTrue(ok);
        (ok,) = _check(_sale(1e18, 474e6));
        assertFalse(ok);
    }

    function test_a_mirror_older_than_the_driver_trusts_voids_the_check() public {
        skip(4 days);
        (bool ok, bytes32 reason) = _check(_sale(1e18, 400e6));
        assertFalse(ok);
        assertEq(reason, "feed");
    }
}
