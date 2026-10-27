// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Closer} from "../src/Closer.sol";
import {CloseBand} from "../src/drivers/CloseBand.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {StationCloses} from "../src/station/StationCloses.sol";
import {ClaimBase} from "./ClaimBase.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A venue whose closes settle in the transaction (as the Robinhood Safe's
/// do) holding several stocks and idle dollars. The driver can't tell that
/// a sale will fail when it is made, so it orders one call per holding and
/// one for the idle dollars, every time. With `maxSale` set it sells at
/// most that much of a holding per close of a stopped station's unwind, at
/// any band, and reports the close blocked beside its calls while more is
/// left, as the Robinhood driver does.
contract HoldingsVenueDriver {
    uint256 private constant WAD = 1e18;

    /// Sold for its worth.
    uint8 internal constant SELLS = 0;
    /// Its transfers are paused by the issuer: the sale reverts.
    uint8 internal constant PAUSED = 1;
    /// The sale burns whatever gas it is given.
    uint8 internal constant BURNS_GAS = 2;
    /// The sale goes through, but only with a lot of gas (a sale across
    /// many ticks, or across an emptied pool to the tick limit): handed
    /// less, it runs out of gas mid-way, as a real swap does.
    uint8 internal constant HUNGRY = 3;
    uint256 public hunger = 1_500_000;
    /// The most of a holding one close of a stopped unwind sells; none if 0.
    uint256 public maxSale;
    /// What the driver's own reads in a close spend.
    uint256 public readCost;

    IERC20 public immutable asset;
    uint256[] public worth;
    uint8[] public kind;
    uint256 public idle;

    constructor(IERC20 asset_) {
        asset = asset_;
    }

    function hold(uint8 kind_, uint256 worth_) external {
        kind.push(kind_);
        worth.push(worth_);
    }

    function setKind(uint256 i, uint8 kind_) external {
        kind[i] = kind_;
    }

    function setIdle(uint256 idle_) external {
        idle = idle_;
    }

    function setHunger(uint256 hunger_) external {
        hunger = hunger_;
    }

    function setMaxSale(uint256 maxSale_) external {
        maxSale = maxSale_;
    }

    function setReadCost(uint256 readCost_) external {
        readCost = readCost_;
    }

    function name() external pure returns (bytes32) {
        return "local";
    }

    function targets() external view returns (address[] memory list) {
        list = new address[](1);
        list[0] = address(this);
    }

    function restricted() external pure returns (bytes32[] memory) {
        return new bytes32[](0);
    }

    function custody() external pure returns (Custody) {
        return Custody.Venue;
    }

    function exitPath() external pure returns (ExitPath) {
        return ExitPath.Contract;
    }

    function valuation() external pure returns (ValuationKind) {
        return ValuationKind.OnChain;
    }

    function approveKey(address, address, uint64) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function revokeKey(address, address) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function deposit(address, uint256) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function withdraw(address, uint256) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    /// One sale per holding, then the idle dollars; blocked only beside its
    /// calls, in a stopped unwind, while a holding has more than `maxSale`
    /// left to sell.
    function close(
        address,
        uint256 fraction,
        uint16 band
    ) external view returns (Call[] memory calls, bool more) {
        _burn(readCost);
        calls = new Call[](worth.length + 1);
        for (uint256 i = 0; i < worth.length; i++) {
            uint256 amount = worth[i] * fraction / WAD;
            if (CloseBand.isStopped(band) && maxSale != 0 && amount > maxSale) {
                amount = maxSale;
                more = true;
            }
            calls[i] = Call(address(this), 0, abi.encodeCall(this.sell, (i, amount)));
        }
        calls[worth.length] = Call(address(this), 0, abi.encodeCall(this.sendIdle, (fraction)));
    }

    function sweep(address, uint256) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function valueOf(address) external view returns (uint256 total, uint256) {
        total = idle;
        for (uint256 i = 0; i < worth.length; i++) {
            total += worth[i];
        }
        return (total, total);
    }

    function sell(uint256 i, uint256 amount) external {
        uint8 k = kind[i];
        if (k == PAUSED) revert("paused");
        if (k == BURNS_GAS) {
            assembly {
                invalid()
            }
        }
        if (k == HUNGRY) _burn(hunger);
        worth[i] -= amount;
        asset.transfer(msg.sender, amount);
    }

    function sendIdle(uint256 fraction) external {
        uint256 part = idle * fraction / WAD;
        idle -= part;
        asset.transfer(msg.sender, part);
    }

    function _burn(uint256 amount) private view {
        uint256 start = gasleft();
        uint256 sink;
        while (start - gasleft() < amount) {
            unchecked {
                sink += uint256(keccak256(abi.encode(sink)));
            }
        }
    }
}

/// A stopped station's full unwind, at the full band, which every holder
/// shares: each call is given exactly a fixed allowance of gas, so whether
/// it goes through never depends on who sends the unwind or with how much.
/// A holding whose sale fails within it (its token's transfers paused,
/// frozen or retired, a pool that can't take the sale, a token that burns
/// its gas) stays where it is, and everything else, the other holdings and
/// the idle dollars, comes home. An unwind sent with too little gas to give
/// a call its allowance is undone whole, whoever makes it, so no sale is
/// ever left for the caller's want of gas while the rest goes ahead. Inside
/// the band a driver's part is still all or nothing.
contract AFullUnwindLeavesBehindOnlyWhatCannotBeSoldTest is ClaimBase {
    uint8 internal constant SELLS = 0;
    uint8 internal constant PAUSED = 1;
    uint8 internal constant BURNS_GAS = 2;
    uint8 internal constant HUNGRY = 3;
    /// EIP-7825's per-transaction gas cap, which Base applies.
    uint256 internal constant TX_GAS_CAP = 16_777_216;
    /// The most tokens the Robinhood driver lists (its `MAX_LISTINGS`).
    uint256 internal constant MAX_LISTINGS = 2;
    /// What the Robinhood driver's reads in a full unwind (its value and
    /// its close) spend, at the most, for that many tokens.
    uint256 internal constant DRIVER_READS = 700_000;
    /// What the worst full unwind leaves of the cap, at the least.
    uint256 internal constant MARGIN = 750_000;

    HoldingsVenueDriver internal holdings;

    /// A halted vault whose station holds, through one driver, `first` and
    /// a saleable stock worth $600, and $300 idle at the venue.
    function _stoppedHolding(uint8 first, uint256 firstWorth) internal {
        _holding(first, firstWorth);
        _stop();
    }

    function _holding(uint8 first, uint256 firstWorth) internal {
        depositAs(vault, alice, 1000e6);
        depositAs(vault, bob, 1000e6);
        pinnedDriverActsAs(address(new HoldingsVenueDriver(IERC20(address(usd)))));
        holdings = HoldingsVenueDriver(address(driver));
        holdings.hold(first, firstWorth);
        holdings.hold(SELLS, 600e6);
        holdings.setIdle(300e6);
        give(address(driver), firstWorth + 900e6);
        vm.prank(executor);
        station.pull(1900e6);
    }

    function _stop() internal {
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
    }

    function _toTheFullBand() internal {
        skip(14 * 7 days);
        assertEq(station.stoppedBand(), 10_000, "the band is not full yet");
    }

    function _local() internal pure returns (bytes32[] memory names) {
        names = new bytes32[](1);
        names[0] = "local";
    }

    function test_a_holding_whose_sale_reverts_leaves_only_itself_behind() public {
        _stoppedHolding(PAUSED, 100e6);
        _toTheFullBand();
        uint256 cashBefore = vault.cash() + usd.balanceOf(address(station));
        (, bool blocked) = station.unwind(1e18);
        assertEq(holdings.worth(1), 0, "the saleable stock was not sold");
        assertEq(holdings.idle(), 0, "the idle dollars stayed at the venue");
        assertEq(holdings.worth(0), 100e6, "the paused stock moved");
        assertEq(vault.cash(), cashBefore + 900e6, "what could be sold did not come home");
        assertTrue(blocked, "an unwind that left a holding behind reported itself done");
        // Still held, and still counted by the driver.
        (uint256 mark,) = holdings.valueOf(address(station));
        assertEq(mark, 100e6);
    }

    function test_a_paused_holding_is_sold_once_its_transfers_resume() public {
        _stoppedHolding(PAUSED, 100e6);
        _toTheFullBand();
        station.unwind(1e18);
        // The issuer lifts the pause.
        holdings.setKind(0, SELLS);
        uint256 cashBefore = vault.cash() + usd.balanceOf(address(station));
        (, bool blocked) = station.unwind(1e18);
        assertEq(holdings.worth(0), 0);
        assertEq(vault.cash(), cashBefore + 100e6);
        assertFalse(blocked);
    }

    /// A token whose sale burns all the gas it is given leaves only itself
    /// behind: the other holdings and the idle dollars come home, through
    /// the unwind, through the driver by name, and with as much gas as a
    /// transaction may carry.
    function test_a_holding_that_burns_its_gas_leaves_only_itself_behind() public {
        _stoppedHolding(BURNS_GAS, 100e6);
        _toTheFullBand();
        uint256 cashBefore = vault.cash() + usd.balanceOf(address(station));
        uint256 snapshot = vm.snapshotState();
        for (uint256 way = 0; way < 3; way++) {
            bool blocked;
            if (way == 0) (, blocked) = station.unwind(1e18);
            else if (way == 1) (, blocked) = station.unwindStopped(_local());
            else (, blocked) = station.unwind{gas: TX_GAS_CAP}(1e18);
            assertEq(holdings.worth(1), 0, "the saleable stock was held by the burning one");
            assertEq(holdings.idle(), 0, "the idle dollars were held by the burning holding");
            assertEq(holdings.worth(0), 100e6, "the burning holding moved");
            assertEq(vault.cash(), cashBefore + 900e6, "what could be sold did not come home");
            assertTrue(blocked, "an unwind that left a holding behind reported itself done");
            vm.revertToState(snapshot);
        }
    }

    /// A sale that needs more than its allowance (a sale of a whole large
    /// holding, a pool griefed with ticks) is left, whatever gas the unwind
    /// is sent with, and never holds anything else.
    function test_a_sale_needing_more_than_its_allowance_leaves_only_itself_behind_whatever_gas_is_sent()
        public
    {
        _stoppedHolding(HUNGRY, 100e6);
        holdings.setHunger(StationCloses.ATTEMPT_GAS + 500_000);
        _toTheFullBand();
        uint256 cashBefore = vault.cash() + usd.balanceOf(address(station));
        uint256[3] memory sends = [uint256(8_000_000), TX_GAS_CAP, 30_000_000];
        for (uint256 i = 0; i < sends.length; i++) {
            uint256 snapshot = vm.snapshotState();
            (, bool blocked) = station.unwind{gas: sends[i]}(1e18);
            assertEq(holdings.worth(0), 100e6, "a sale over its allowance was made");
            assertEq(holdings.worth(1), 0, "the saleable stock was held by a sale over its allowance");
            assertEq(holdings.idle(), 0, "the idle dollars were held by a sale over its allowance");
            assertEq(vault.cash(), cashBefore + 900e6);
            assertTrue(blocked);
            vm.revertToState(snapshot);
        }
    }

    /// Too little gas to give a call its allowance undoes the whole
    /// unwind, through every door: the unwind, the driver by name, and the
    /// Closer. Nothing on the way catches it. With enough, the sale is made.
    function test_too_little_gas_reverts_the_whole_unwind_rather_than_skipping_anything() public {
        _stoppedHolding(HUNGRY, 100e6);
        holdings.setHunger(2_500_000);
        _toTheFullBand();
        uint256 cashBefore = vault.cash();
        vm.expectRevert(StationCloses.ShortOfGas.selector);
        station.unwind{gas: 3_000_000}(1e18);
        vm.expectRevert(StationCloses.ShortOfGas.selector);
        station.unwindStopped{gas: 3_000_000}(_local());
        Closer closer = new Closer();
        vm.expectRevert(StationCloses.ShortOfGas.selector);
        closer.closeHalted{gas: 3_000_000}(address(vault));
        // Enough for the first call's allowance, not the next one's.
        vm.expectRevert(StationCloses.ShortOfGas.selector);
        station.unwind{gas: 5_000_000}(1e18);
        assertEq(holdings.worth(0), 100e6);
        assertEq(holdings.worth(1), 600e6, "a sale was made beside one left for want of gas");
        assertEq(holdings.idle(), 300e6, "the idle dollars moved beside a sale left for want of gas");
        assertEq(vault.cash(), cashBefore);
        // With enough gas it is sold.
        (, bool blocked) = station.unwind{gas: 12_000_000}(1e18);
        assertEq(holdings.worth(0), 0, "the hungry sale was not made with enough gas");
        assertFalse(blocked);
    }

    function test_inside_the_band_one_failing_sale_still_closes_nothing() public {
        _stoppedHolding(PAUSED, 100e6);
        assertLt(station.stoppedBand(), 10_000);
        (, bool blocked) = station.unwind(1e18);
        assertTrue(blocked);
        assertEq(holdings.worth(1), 600e6, "a stock was sold inside the band beside a failing sale");
        assertEq(holdings.idle(), 300e6, "dollars left the venue inside the band beside a failing sale");
    }

    /// Whatever gas the unwind is sent with, a sale that fits its allowance
    /// is never the one left behind while the rest of the driver's part
    /// goes through: either it is made, or the whole unwind is undone.
    function test_a_caller_cannot_have_a_sale_skipped_by_sending_little_gas() public {
        _stoppedHolding(HUNGRY, 100e6);
        holdings.setHunger(2_500_000);
        _toTheFullBand();
        _neverSkippedForWantOfGas();
    }

    /// The same in the first week, through a driver selling part of a
    /// holding per close (each call then made with its allowance).
    function test_in_the_first_week_a_caller_cannot_have_a_sale_skipped_by_sending_little_gas() public {
        _stoppedHolding(HUNGRY, 100e6);
        holdings.setHunger(2_500_000);
        holdings.setMaxSale(100e6);
        assertLt(station.stoppedBand(), 10_000);
        _neverSkippedForWantOfGas();
    }

    function _neverSkippedForWantOfGas() internal {
        bool sold;
        for (uint256 gas = 300_000; gas <= 14_000_000; gas += 200_000) {
            uint256 snapshot = vm.snapshotState();
            try station.unwind{gas: gas}(1e18) {} catch {}
            bool others = holdings.worth(1) < 600e6 || holdings.idle() == 0;
            bool hungry = holdings.worth(0) == 0;
            assertTrue(!others || hungry, "a sale was left for want of gas while the others went");
            if (hungry) sold = true;
            vm.revertToState(snapshot);
        }
        assertTrue(sold, "never sent with enough gas to sell anything");
    }

    /// Inside the band a driver's part short of gas is undone whole: none
    /// of it moves, and the unwind isn't done.
    function test_inside_the_band_a_part_short_of_gas_moves_nothing_of_it() public {
        _stoppedHolding(HUNGRY, 100e6);
        holdings.setHunger(5_000_000);
        assertLt(station.stoppedBand(), 10_000);
        (, bool blocked) = station.unwind{gas: 3_000_000}(1e18);
        assertTrue(blocked);
        assertEq(holdings.worth(0), 100e6);
        assertEq(holdings.worth(1), 600e6, "a stock was sold inside the band beside a part short of gas");
        assertEq(holdings.idle(), 300e6, "dollars left the venue inside the band beside a part short of gas");
    }

    /// The band a stopped unwind gives its driver, week by week: the exit
    /// band marked as a stopped unwind's for the first week, doubling each
    /// week until full, then the full band counting on by one each week,
    /// while the station's own `stoppedBand` stays at the full band.
    function test_a_stopped_unwind_gives_its_driver_its_band_marked_and_counting_on_past_the_full_band()
        public
    {
        _stoppedHolding(PAUSED, 100e6);
        uint256 start = station.stoppedAt();
        assertEq(vault.exitBandBps(), 100, "this schedule is the 1% band's");
        uint16[8] memory widening = [uint16(100), 200, 400, 800, 1600, 3200, 6400, 10_000];
        for (uint256 week = 0; week < 20; week++) {
            vm.warp(start + week * 7 days + 1 hours);
            uint256 expected = week < 8 ? widening[week] : 10_000 + (week - 7);
            assertEq(station.stoppedBand(), expected > 10_000 ? 10_000 : expected, "the station's own band");
            vm.expectCall(
                address(driver),
                abi.encodeCall(IDriver.close, (address(station), 1e18, uint16(CloseBand.STOPPED | expected)))
            );
            station.unwind(1e18);
        }
    }

    /// In the first week, a driver that sells part of a holding per close
    /// of a stopped unwind has its calls made each on its own, as at the
    /// full band: a holding larger than one sale is sold a slice per
    /// unwind, the idle dollars come home with the first, and a holding
    /// whose sale fails leaves only itself behind.
    function test_in_the_first_week_a_driver_selling_part_per_close_has_each_call_made_on_its_own() public {
        _stoppedHolding(PAUSED, 100e6);
        holdings.setMaxSale(250e6);
        assertLt(station.stoppedBand(), 10_000);
        uint256 cashBefore = vault.cash() + usd.balanceOf(address(station));
        (, bool blocked) = station.unwind(1e18);
        assertTrue(blocked);
        assertEq(holdings.worth(0), 100e6, "the paused stock moved");
        assertEq(holdings.worth(1), 350e6, "other than one sale's maximum was sold");
        assertEq(holdings.idle(), 0, "the idle dollars stayed at the venue");
        assertEq(vault.cash(), cashBefore + 250e6 + 300e6, "what was sold did not come home");
    }

    /// A holding larger than one sale's maximum is sold over several
    /// unwinds: each sells the maximum and reports itself not done, the
    /// idle dollars and the smaller holding come home with the first, and
    /// the last brings home the rest and reports itself done.
    function test_a_full_unwind_of_a_holding_larger_than_one_sale_completes_over_several_unwinds() public {
        _stoppedHolding(SELLS, 100e6);
        holdings.setMaxSale(250e6);
        _toTheFullBand();
        uint256 cashBefore = vault.cash() + usd.balanceOf(address(station));
        (, bool blocked) = station.unwind(1e18);
        assertTrue(blocked, "an unwind that left some of a holding unsold reported itself done");
        assertEq(holdings.worth(0), 0);
        assertEq(holdings.worth(1), 350e6, "other than one sale's maximum was sold");
        assertEq(holdings.idle(), 0);
        assertEq(vault.cash(), cashBefore + 100e6 + 250e6 + 300e6, "what was sold did not come home");
        // Still held, and still counted by the driver, never higher.
        (uint256 mark,) = holdings.valueOf(address(station));
        assertEq(mark, 350e6);
        (, blocked) = station.unwind(1e18);
        assertTrue(blocked);
        assertEq(holdings.worth(1), 100e6);
        (, blocked) = station.unwind(1e18);
        assertFalse(blocked, "the unwind that sold the last of it reported itself blocked");
        assertEq(holdings.worth(1), 0);
        assertEq(vault.cash(), cashBefore + 1000e6, "the whole holding did not come home");
    }

    /// The worst full unwind through a driver listing as many tokens as the
    /// Robinhood driver may: every call (an approval and a sale per token,
    /// and the idle dollars) spending all its allowance, after the driver's
    /// reads, fits EIP-7825's per-transaction cap with room to spare.
    function test_the_worst_full_unwind_fits_the_per_transaction_gas_cap() public {
        _worstUnwind(false);
    }

    /// The same in the first week, through a driver selling part of each
    /// holding per close (each call then made with its allowance there
    /// too): it fits the cap the same.
    function test_the_worst_first_week_unwind_fits_the_per_transaction_gas_cap() public {
        _worstUnwind(true);
    }

    function _worstUnwind(bool firstWeek) internal {
        depositAs(vault, alice, 1000e6);
        pinnedDriverActsAs(address(new HoldingsVenueDriver(IERC20(address(usd)))));
        holdings = HoldingsVenueDriver(address(driver));
        uint256 calls = 2 * MAX_LISTINGS;
        for (uint256 i = 0; i < calls; i++) {
            holdings.hold(BURNS_GAS, 1e6);
        }
        holdings.setIdle(300e6);
        holdings.setReadCost(DRIVER_READS);
        give(address(driver), calls * 1e6 + 300e6);
        vm.prank(executor);
        station.pull(300e6);
        _stop();
        if (firstWeek) {
            holdings.setMaxSale(1);
            assertLt(station.stoppedBand(), 10_000);
        } else {
            _toTheFullBand();
        }
        // The least gas the unwind goes through with.
        uint256 low = 10_000_000;
        uint256 high = TX_GAS_CAP + 4_000_000;
        while (high - low > 10_000) {
            uint256 mid = (low + high) / 2;
            uint256 snapshot = vm.snapshotState();
            try station.unwind{gas: mid}(1e18) {
                high = mid;
            } catch {
                low = mid;
            }
            vm.revertToState(snapshot);
        }
        emit log_named_uint(
            firstWeek
                ? "the least gas the worst first-week unwind goes through with"
                : "the least gas the worst full unwind goes through with",
            high
        );
        assertLe(high, TX_GAS_CAP - MARGIN, "the worst full unwind does not fit the cap with room to spare");
        (, bool blocked) = station.unwind{gas: TX_GAS_CAP - MARGIN}(1e18);
        assertTrue(blocked);
        assertEq(holdings.idle(), 0, "the idle dollars did not come home");
        for (uint256 i = 0; i < calls; i++) {
            assertEq(holdings.worth(i), 1e6);
        }
    }
}
