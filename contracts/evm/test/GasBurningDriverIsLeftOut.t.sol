// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {FakeVault, StationBase} from "./Station.t.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A pinned driver that burns its gas (a venue whose read loops without
/// bound, or a driver written to) can't keep the other venues' money or the
/// idle dollars at a stopped station: anyone may unwind a stopped station
/// through the drivers they name, leaving the burner out, and the send home
/// fits an ordinary block.
contract GasBurningDriverIsLeftOutTest is StationBase {
    /// HyperEVM's small-block gas limit.
    uint256 internal constant SMALL_BLOCK = 2_000_000;
    /// How many markets the honest venue holds. Each costs the stand-in
    /// driver about 45k gas to close; a real venue's driver spends more per
    /// position (a Safe transfer, a CoreWriter order and a precompile read).
    uint256 internal constant MARKETS = 24;

    /// The burner, approved under its own name like any driver.
    function _approveBurner() internal {
        BurningDriver burner = new BurningDriver();
        vm.prank(admin);
        attestation.proposeDriver("burning", address(burner));
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

    function _market(uint256 i) internal pure returns (bytes32) {
        return keccak256(abi.encode("M", i));
    }

    /// Spot positions held at the stand-in venue, as a basket strategy would,
    /// with some idle dollars left at the station.
    function _trade(Station two) internal {
        vm.startPrank(operator);
        for (uint256 i = 0; i < MARKETS; i++) {
            venue.listMarket(_market(i), false, 1000e6);
        }
        vm.stopPrank();
        vm.startPrank(executor);
        two.pull(100_000e6);
        two.approveKey("local", executor);
        two.deposit("local", 80_000e6);
        OrderRequest[] memory orders = new OrderRequest[](MARKETS);
        for (uint256 i = 0; i < MARKETS; i++) {
            orders[i] = OrderRequest({
                market: _market(i),
                isBuy: true,
                size: 3e18,
                limitPrice: 1000e6,
                reduceOnly: false
            });
        }
        venue.place(address(two), orders);
        vm.stopPrank();
    }

    function _stop(Station two, FakeVault other) internal {
        other.halt();
        skip(other.haltGrace());
        two.stop();
    }

    function _names(bytes32 first, bytes32 second) internal pure returns (bytes32[] memory names) {
        names = new bytes32[](2);
        names[0] = first;
        names[1] = second;
    }

    /// What the honest station's stopped unwind costs on its own.
    function _honestCost() internal returns (uint256 cost) {
        uint256 snap = vm.snapshotState();
        (Station alone, FakeVault other) = _stationWith(locals());
        _trade(alone);
        _stop(alone, other);
        uint256 before = gasleft();
        alone.unwind(1e18);
        cost = before - gasleft();
        assertEq(other.cash(), 100_000e6, "the honest unwind alone did not bring everything home");
        vm.revertToState(snap);
    }

    function _only(bytes32 name) internal pure returns (bytes32[] memory names) {
        names = new bytes32[](1);
        names[0] = name;
    }

    function test_a_burning_driver_listed_first_is_left_out_and_the_rest_comes_home() public {
        _approveBurner();
        assertLt(_honestCost(), SMALL_BLOCK * 3 / 4);
        (Station two, FakeVault other) = _stationWith(_names("burning", "local"));
        _trade(two);
        _stop(two, other);

        (bool ok,) =
            address(two).call{gas: SMALL_BLOCK}(abi.encodeCall(Station.unwindStopped, (_only("local"))));
        assertTrue(ok, "the stopped unwind through the working driver reverted");
        assertEq(venue.positionOf(address(two), _market(0)).size, 0, "the working venue was not unwound");
        assertEq(usd.balanceOf(address(two)), 0, "idle dollars stayed at the station");
        assertGe(other.cash(), 99_000e6, "the money did not come home");
    }

    function test_the_idle_dollars_come_home_with_no_driver_named() public {
        _approveBurner();
        (Station two, FakeVault other) = _stationWith(_names("local", "burning"));
        _trade(two);
        _stop(two, other);

        (bool ok,) =
            address(two).call{gas: SMALL_BLOCK}(abi.encodeCall(Station.unwindStopped, (new bytes32[](0))));
        assertTrue(ok, "sending the idle dollars home reverted");
        assertEq(usd.balanceOf(address(two)), 0, "idle dollars stayed at the station");
        assertGe(other.cash(), 20_000e6, "the idle dollars did not come home");
    }

    function test_only_a_stopped_station_unwinds_by_name() public {
        vm.expectRevert(Station.NotHalted.selector);
        station.unwindStopped(locals());
    }
}

/// A driver whose reads run until the gas is gone: a venue whose list grew
/// without bound, or a driver written to do exactly this.
contract BurningDriver is IDriver {
    uint256 private _sink;

    function _burn() private view {
        uint256 x;
        while (true) {
            x = uint256(keccak256(abi.encode(x, _sink)));
        }
    }

    function name() external pure returns (bytes32) {
        return "burning";
    }

    function targets() external pure returns (address[] memory) {
        return new address[](0);
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

    function close(address, uint256, uint16) external view returns (Call[] memory, bool) {
        _burn();
        return (new Call[](0), false);
    }

    function sweep(address, uint256) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function valueOf(address) external view returns (uint256, uint256) {
        _burn();
        return (0, 0);
    }
}
