// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {FakeVault, StationBase} from "./Station.t.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A halted station stops whatever its drivers do: each step of the
/// best-effort revoke has a bounded budget, so a driver that burns its gas
/// while being revoked can't keep the station from stopping, and so from being
/// unwound by anyone.
contract StopUnderAGasBurningDriverTest is StationBase {
    /// A large block: Base's limit is above this.
    uint256 internal constant BIG_BLOCK = 100_000_000;

    function _names(bytes32 first, bytes32 second) internal pure returns (bytes32[] memory names) {
        names = new bytes32[](2);
        names[0] = first;
        names[1] = second;
    }

    function test_a_driver_that_burns_gas_while_being_revoked_cannot_keep_the_station_from_stopping()
        public
    {
        GasSink sink = new GasSink();
        SlowRevokeDriver slow = new SlowRevokeDriver(address(sink));
        vm.prank(admin);
        attestation.proposeDriver("slow", address(slow));
        skip(TIMELOCK);

        FakeVault other = new FakeVault(IERC20(address(usd)));
        Station two = Station(
            ownStations.createHome(address(other), BUNDLE, _names("local", "slow"), bridges(), others(), 100)
        );
        other.setStation(address(two));
        give(address(other), 100_000e6);
        other.fund(100_000e6);
        blessExecutorKey(executor, address(other), BUNDLE);

        // The executor runs the strategy: money at the honest venue, a key
        // approved through both drivers (the slow one behaves until now).
        vm.startPrank(executor);
        two.pull(100_000e6);
        two.approveKey("local", executor);
        two.approveKey("slow", executor);
        two.deposit("local", 60_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 0.5e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(two), orders);
        vm.stopPrank();

        // The driver turns hostile (or its venue's read grows without bound).
        slow.setHostile(true);

        other.halt();
        skip(other.haltGrace());

        // Anyone should be able to stop the station once the grace has passed.
        vm.prank(alice);
        (bool ok,) = address(two).call{gas: BIG_BLOCK}(abi.encodeWithSignature("stop()"));
        assertTrue(ok, "a gas-burning driver kept the halted station from stopping");
        assertTrue(two.stopped());

        // And then unwind the honest venue by name, leaving the burner out.
        bytes32[] memory onlyLocal = new bytes32[](1);
        onlyLocal[0] = "local";
        vm.prank(alice);
        two.unwindStopped(onlyLocal);
        assertGt(other.cash(), 99_000e6, "the honest venue's money did not come home");
    }

    /// The control: a venue that burns gas in its revoke is one sink and
    /// leaves the station 1/64 of the block, which stops fine.
    function test_a_venue_that_burns_gas_in_its_revoke_cannot_keep_the_station_from_stopping() public {
        GasSink sink = new GasSink();
        SlowRevokeDriver slow = new SlowRevokeDriver(address(sink));
        slow.setVenueOnly(true);
        vm.prank(admin);
        attestation.proposeDriver("slow", address(slow));
        skip(TIMELOCK);

        FakeVault other = new FakeVault(IERC20(address(usd)));
        Station two = Station(
            ownStations.createHome(address(other), BUNDLE, _names("local", "slow"), bridges(), others(), 100)
        );
        other.setStation(address(two));
        give(address(other), 100_000e6);
        other.fund(100_000e6);
        blessExecutorKey(executor, address(other), BUNDLE);
        vm.startPrank(executor);
        two.approveKey("slow", executor);
        vm.stopPrank();
        slow.setHostile(true);

        other.halt();
        skip(other.haltGrace());
        vm.prank(alice);
        (bool ok,) = address(two).call{gas: BIG_BLOCK}(abi.encodeWithSignature("stop()"));
        assertTrue(ok, "a venue burning gas in revoke kept the station from stopping");
    }
}

/// Burns everything it is given.
contract GasSink {
    uint256 private _x;

    fallback() external {
        uint256 x;
        while (true) {
            x = uint256(keccak256(abi.encode(x, _x)));
        }
    }
}

/// Honest until told otherwise. Hostile, its `revokeKey` and `targets` views
/// each spend nearly all the gas they are given and return, and the revoke
/// call they prepare goes to a contract that burns the rest.
contract SlowRevokeDriver is IDriver {
    address public immutable sink;
    bool public hostile;
    bool public venueOnly;

    constructor(address sink_) {
        sink = sink_;
    }

    function setHostile(bool on) external {
        hostile = on;
    }

    function setVenueOnly(bool on) external {
        venueOnly = on;
    }

    /// Spend nearly everything, then return normally.
    function _spin() private view {
        assembly ("memory-safe") {
            for {} gt(gas(), 20000) {} { mstore(0, keccak256(0, 32)) }
        }
    }

    function name() external pure returns (bytes32) {
        return "slow";
    }

    function targets() external view returns (address[] memory list) {
        if (hostile && !venueOnly) _spin();
        list = new address[](1);
        list[0] = sink;
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

    function revokeKey(address, address) external view returns (Call[] memory calls) {
        if (hostile && !venueOnly) _spin();
        if (!hostile) return new Call[](0);
        calls = new Call[](1);
        calls[0] = Call(sink, 0, hex"01");
    }

    function deposit(address, uint256) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function withdraw(address, uint256) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function close(address, uint256, uint16) external pure returns (Call[] memory, bool) {
        return (new Call[](0), false);
    }

    function sweep(address, uint256) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function valueOf(address) external pure returns (uint256, uint256) {
        return (0, 0);
    }
}
