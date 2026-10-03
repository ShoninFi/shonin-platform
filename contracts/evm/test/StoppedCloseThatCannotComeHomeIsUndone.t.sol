// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {Station} from "../src/Station.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {ILocalVenue, OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {IStation} from "../src/interfaces/IStation.sol";
import {FakeVault, StationBase} from "./Station.t.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A stopped station's full unwind closes each venue on its own, and a close
/// whose worth doesn't come home (a venue that keeps the proceeds) is undone
/// as a short sweep: that venue's positions stay as they were, the driver is
/// named as failed, and the idle dollars still come home.
contract StoppedCloseThatCannotComeHomeIsUndoneTest is StationBase {
    function _approveKeeper() internal {
        KeepingVenueDriver keeper = new KeepingVenueDriver(new LocalDriver(venue, IERC20(address(usd))));
        vm.prank(admin);
        attestation.proposeDriver("keeping", address(keeper));
        skip(TIMELOCK);
    }

    function _stationWith(bytes32[] memory names) internal returns (Station two, FakeVault other) {
        other = new FakeVault(IERC20(address(usd)));
        two = Station(ownStations.createHome(address(other), BUNDLE, names, bridges(), others(), 100));
        other.setStation(address(two));
        give(address(other), 100_000e6);
        other.fund(100_000e6);
        blessExecutorKey(executor, address(other), BUNDLE);
    }

    /// A spot position at the venue, with some idle dollars left at the
    /// station.
    function _trade(Station two, bytes32 name) internal {
        vm.startPrank(executor);
        two.pull(100_000e6);
        two.approveKey(name, executor);
        two.deposit(name, 80_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(two), orders);
        vm.stopPrank();
    }

    function _only(bytes32 name) internal pure returns (bytes32[] memory names) {
        names = new bytes32[](1);
        names[0] = name;
    }

    function test_a_stopped_close_whose_worth_stays_at_the_venue_is_undone_and_the_idle_dollars_come_home()
        public
    {
        _approveKeeper();
        (Station two, FakeVault other) = _stationWith(_only("keeping"));
        _trade(two, "keeping");
        other.halt();
        skip(other.haltGrace());
        two.stop();
        int256 size = venue.positionOf(address(two), SPOT).size;
        int256 cash = venue.cashOf(address(two));
        uint256 idle = usd.balanceOf(address(two));
        assertGt(idle, 0, "nothing idle at the station");

        vm.expectEmit(true, false, false, false, address(two));
        emit IStation.DriverFailed("keeping");
        (uint256 sentHome, bool blocked) = two.unwind(1e18);
        assertTrue(blocked, "the unwind did not report the failed driver");
        assertEq(
            venue.positionOf(address(two), SPOT).size, size, "a close that brought nothing home was kept"
        );
        assertEq(venue.cashOf(address(two)), cash, "the venue's cash moved for a close that was undone");
        assertEq(sentHome, idle, "other than the idle dollars came home");
        assertEq(usd.balanceOf(address(two)), 0, "the idle dollars stayed at the station");
        assertEq(other.cash(), idle, "the idle dollars were not booked at home");
    }
}

/// The stand-in venue's driver, with a venue that keeps what a close
/// realises: its sweep brings nothing home.
contract KeepingVenueDriver is IDriver {
    LocalDriver internal immutable honest;

    constructor(LocalDriver honest_) {
        honest = honest_;
    }

    function name() external pure returns (bytes32) {
        return "keeping";
    }

    function targets() external view returns (address[] memory) {
        return honest.targets();
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

    function approveKey(address station, address key, uint64 until) external view returns (Call[] memory) {
        return honest.approveKey(station, key, until);
    }

    function revokeKey(address station, address key) external view returns (Call[] memory) {
        return honest.revokeKey(station, key);
    }

    function deposit(address station, uint256 amount) external view returns (Call[] memory) {
        return honest.deposit(station, amount);
    }

    function withdraw(address station, uint256 amount) external view returns (Call[] memory) {
        return honest.withdraw(station, amount);
    }

    function close(
        address station,
        uint256 fraction,
        uint16 band
    ) external view returns (Call[] memory, bool) {
        return honest.close(station, fraction, band);
    }

    function sweep(address, uint256) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function valueOf(address station) external view returns (uint256, uint256) {
        return honest.valueOf(station);
    }
}
