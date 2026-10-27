// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {CloseBand} from "../../src/drivers/CloseBand.sol";
import {IUniswapV3Pool} from "../../src/drivers/robinhood/RobinhoodChain.sol";
import {UniswapPrice} from "../../src/drivers/robinhood/UniswapPrice.sol";
import {RobinhoodPoolsForkTest} from "./AStockPoolHeldOffItsPriceCannotSellBelowTheFeedLessItsDiscount.t.sol";
import {console2} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// Anyone may send a stopped station's unwind, and the pool can be pushed
/// within the transaction that sells: a sender can push the pool, bid, have
/// the station sell to them, and buy the pool back. At any band, the
/// station's sale is at most `maxSale` a close and never below its floor:
/// the higher of the feed less the band (at most 10%) and the pool's
/// time-weighted price less `SALE_SPACINGS`, less the pool's fee, neither of
/// which the push moves. However many times the unwind is called within the
/// transaction, that floor is the most the sender takes it at. On the real
/// NVDA/USDG pool and feed, at each week's band before the full one. Runs
/// only with `ROBINHOOD_FORK_URL` (on a weekday, the feed fresh).
contract AStoppedStationsSaleAtAnyBandCannotBeTakenBelowItsFloorInOneTransactionTest is
    RobinhoodPoolsForkTest
{
    uint256 internal constant HOLDING = 5000 ether;
    uint256 internal constant SCALE = 1e20;

    /// The band's floor, in USDG (6 dp) per NVDA, before the pool's fee:
    /// the higher of the feed less the band (at most 10%) and the
    /// time-weighted price less the spacings.
    function _floorAt(uint256 band, uint256 feedPrice) internal view returns (uint256) {
        uint256 discount = band < driver.FULL_BAND_FEED_BPS() ? band : driver.FULL_BAND_FEED_BPS();
        uint256 atFeed = feedPrice * (10_000 - discount) / 10_000;
        int24 mean = UniswapPrice.meanTick(IUniswapV3Pool(NVDA_POOL), WINDOW, false);
        uint256 atTwap = UniswapPrice.quoteAtTick(
            mean + int24(int256(driver.SALE_SPACINGS())) * SPACING, 1 ether, NVDA, USDG
        );
        return atFeed > atTwap ? atFeed : atTwap;
    }

    /// The pool's sqrt price at a price of one NVDA in USDG (6 dp): USDG is
    /// token0, so the pool's price is NVDA per USDG.
    function _sqrtAt(uint256 price6) internal pure returns (uint160) {
        return uint160(Math.sqrt(Math.mulDiv(SCALE, 1 << 192, price6 * 100)));
    }

    /// One transaction: push the pool to `target` (USDG per NVDA), bid just
    /// above it, have the station's unwind called again and again at
    /// `band`, take the bid out and buy the pool back. What the station
    /// sold, what it fetched, and the most one close sold.
    function _sandwich(
        uint16 band,
        uint256 target
    ) internal returns (uint256 sold, uint256 fetched, uint256 mostInOneClose) {
        deal(NVDA, safe, HOLDING);
        (, startTick,,,,,) = IUniswapV3Pool(NVDA_POOL).slot0();
        _swap(NVDA, USDG, 1_000_000 ether, _sqrtAt(target));
        (, int24 tick,,,,,) = IUniswapV3Pool(NVDA_POOL).slot0();
        int24 lower = (tick + 1) / SPACING * SPACING;
        if (lower < tick + 1) lower += SPACING;
        _bid(NVDA_POOL, lower, lower + 40 * SPACING);
        (sold, fetched, mostInOneClose) = _sellAll(NVDA, _stopped(band));
        _unbid(NVDA_POOL);
        _swap(USDG, NVDA, 1_000_000_000e6, UniswapPrice.sqrtRatioAtTick(startTick));
    }

    /// Where the pool was before the push.
    int24 internal startTick;

    /// A stopped unwind's band, as the station gives it to its drivers.
    function _stopped(uint16 band) internal pure returns (uint16) {
        return uint16(CloseBand.STOPPED | band);
    }

    /// Pushed to the feed less each week's band (2% to 64%), where a sale
    /// with only a minimum out at the feed less the band would fill: the
    /// station sells no unit below its floor and at most `maxSale` a close;
    /// from the band past the spacings, the push is past the floor and
    /// nothing is sold at all.
    function test_on_the_real_nvda_pool_a_push_to_each_weeks_band_takes_no_stopped_sale_below_its_floor()
        public
        onFork
    {
        vm.warp(vm.getBlockTimestamp() + WINDOW);
        (uint256 feedPrice, bool fresh) = _feed(NVDA_FEED);
        assertTrue(fresh, "the feed is not fresh at this block");
        uint16[7] memory bands = [uint16(100), 200, 400, 800, 1600, 3200, 6400];
        for (uint256 i = 0; i < bands.length; i++) {
            uint256 snapshot = vm.snapshotState();
            _pushToTheBand(bands[i], feedPrice);
            vm.revertToState(snapshot);
        }
    }

    function _pushToTheBand(uint16 band, uint256 feedPrice) internal {
        uint256 floor = _floorAt(band, feedPrice) * 9995 / 10_000;
        (uint256 sold, uint256 fetched, uint256 most) =
            _sandwich(band, feedPrice * (10_000 - band + 15) / 10_000);
        uint256 price = sold == 0 ? 0 : fetched * 1 ether / sold;
        console2.log("band (bps) / sold (1e18) / price (USDG, 6 dp)", band, sold, price);
        console2.log("band (bps) / the floor less the fee / the feed", band, floor, feedPrice);
        if (sold != 0) assertGe(price, floor, "sold below the band's floor");
        assertLe(most, _maxSale(NVDA), "one close sold more than one sale's maximum");
    }

    /// Pushed to just short of the floor and called until nothing more
    /// sells, within one transaction: the sender takes the station's NVDA a
    /// slice at a time, never below the floor, at each week's band.
    function test_on_the_real_nvda_pool_the_most_a_sender_takes_in_one_transaction_is_the_floor()
        public
        onFork
    {
        vm.warp(vm.getBlockTimestamp() + WINDOW);
        (uint256 feedPrice,) = _feed(NVDA_FEED);
        uint16[4] memory bands = [uint16(100), 200, 1600, 6400];
        for (uint256 i = 0; i < bands.length; i++) {
            uint256 snapshot = vm.snapshotState();
            _pushToTheFloor(bands[i], feedPrice);
            vm.revertToState(snapshot);
        }
    }

    function _pushToTheFloor(uint16 band, uint256 feedPrice) internal {
        uint256 floor = _floorAt(band, feedPrice);
        (uint256 sold, uint256 fetched, uint256 most) = _sandwich(band, floor * 10_015 / 10_000);
        uint256 price = fetched * 1 ether / sold;
        uint256 under = feedPrice > price ? (feedPrice - price) * 10_000 / feedPrice : 0;
        console2.log("band (bps) / sold (1e18) / price (USDG, 6 dp)", band, sold, price);
        console2.log("band (bps) / under the feed (bps) / most in one close", band, under, most);
        assertGt(sold, 0, "nothing sold short of the floor");
        assertLe(most, _maxSale(NVDA), "one close sold more than one sale's maximum");
        assertGe(price, floor * 9995 / 10_000, "sold below the band's floor");
    }
}
