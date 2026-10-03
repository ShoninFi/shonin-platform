// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {Vault} from "../../src/Vault.sol";
import {OrderRequest} from "../../src/interfaces/ILocalVenue.sol";
import {Fixture} from "../Fixture.sol";

/// A stopped station's full unwind is bounded by the exit band for the
/// first week after the stop; from then on the band doubles each week until
/// any price is accepted. Only time moves it, and a running station's band
/// never widens.
contract StoppedBandTest is Fixture {
    uint256 internal constant WAD = 1e18;
    uint256 internal constant BPS = 10_000;

    Vault internal vault;
    Station internal station;
    uint256 internal band;

    function setUp() public override {
        super.setUp();
        (vault,) = openVault();
        station = stationOf(vault);
        band = vault.exitBandBps();
        give(address(venue), 10_000_000e6);
        depositAs(vault, alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(60_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 60_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 0.5e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
    }

    /// The band the stop's statement gives for `elapsed` after it.
    function bandAfter(uint256 elapsed) internal view returns (uint256 expected) {
        expected = band;
        if (elapsed < 7 days) return expected;
        if (expected == 0) expected = 1;
        for (uint256 weeks_ = elapsed / 7 days; weeks_ > 0 && expected < BPS; weeks_--) {
            expected *= 2;
        }
        if (expected > BPS) expected = BPS;
    }

    function stopStation() internal {
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        value(vault);
    }

    /// forge-config: default.fuzz.runs = 512
    function testFuzz_a_stopped_stations_band_holds_a_week_then_doubles_each_week(uint32 elapsed) public {
        stopStation();
        skip(elapsed);
        assertEq(
            station.stoppedBand(), bandAfter(elapsed), "the band is not what the time since the stop gives"
        );
    }

    /// forge-config: default.fuzz.runs = 256
    function testFuzz_a_running_stations_band_never_widens(uint32 elapsed) public {
        skip(elapsed);
        assertEq(station.stoppedBand(), band, "a running station's band widened");
        vm.prank(admin);
        vault.kill();
        skip(uint256(elapsed) + HALT_GRACE);
        assertEq(station.stoppedBand(), band, "a halted station's band widened before it stopped");
    }

    /// With the bid held `spreadBps` under the venue's price, a stopped
    /// station's full unwind is blocked exactly while the spread is wider
    /// than the band as it has widened, and sells everything once it isn't.
    /// forge-config: default.fuzz.runs = 512
    function testFuzz_the_widened_band_bounds_a_stopped_stations_full_unwind(
        uint16 spreadBps,
        uint8 weeks_
    ) public {
        spreadBps = uint16(bound(spreadBps, 0, 9999));
        stopStation();
        uint256 elapsed = uint256(weeks_ % 16) * 7 days;
        skip(elapsed);
        uint256 bid = PRICE * (BPS - spreadBps) / BPS;
        vm.startPrank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, bid, PRICE);
        venue.setPrices(PERP, PRICE, PRICE, bid, PRICE);
        vm.stopPrank();
        uint256 expected = bandAfter(elapsed);
        // The driver sells a long at the bid and refuses one under the band's
        // floor of the venue's price; at the full band it takes any price.
        bool outside = expected < BPS && bid < ceilDiv(PRICE * (BPS - expected), BPS);
        uint256 cash = vault.cash();
        (uint256 sentHome, bool blocked) = station.unwind(WAD);
        assertEq(blocked, outside, "the unwind was blocked other than by the widened band");
        if (outside) {
            assertEq(sentHome, 0, "a blocked unwind sent something home");
            assertEq(uint256(venue.positionOf(address(station), SPOT).size), 0.5e18, "a blocked unwind sold");
        } else {
            assertEq(
                venue.positionOf(address(station), SPOT).size, 0, "an unwind inside the band left a position"
            );
            assertEq(usd.balanceOf(address(station)), 0, "an unwind inside the band left idle dollars");
            assertEq(vault.cash(), cash + sentHome, "what came home was not booked as cash");
        }
    }

    function ceilDiv(uint256 a, uint256 b) internal pure returns (uint256) {
        return (a + b - 1) / b;
    }
}
