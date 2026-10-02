// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {ILocalVenue, OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A station always stops, whatever its pinned driver returns when asked to
/// revoke a key: a list of calls that would burn more than a block, or bytes
/// that don't decode. Each best-effort revoke runs inside one self-call with
/// one fixed budget and at most a few calls, so any failure inside it is
/// caught, and anyone can then unwind the station.
contract StationStopsWhateverItsDriverReturnsTest is Fixture {
    /// Base's block gas limit is below this.
    uint256 internal constant BIG_BLOCK = 200_000_000;

    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function deployed() internal {
        depositAs(vault, alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(90_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 60_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
    }

    function halted() internal {
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
    }

    function test_a_driver_returning_many_burning_calls_cannot_keep_the_station_from_stopping() public {
        deployed();
        GasSink sink = new GasSink();
        // 300 calls at 1,000,000 each is 300M gas: more than any block.
        pinnedDriverActsAs(
            address(new ManyCallsRevokeDriver(venue, IERC20(address(usd)), address(sink), 300))
        );
        halted();

        vm.prank(bob);
        (bool ok,) = address(station).call{gas: BIG_BLOCK}(abi.encodeWithSignature("stop()"));
        assertTrue(ok, "a pinned driver kept the halted station from stopping (many calls)");
        assertTrue(station.stopped());
    }

    function test_a_driver_whose_revoke_returns_undecodable_bytes_cannot_keep_the_station_from_stopping()
        public
    {
        deployed();
        pinnedDriverActsAs(address(new GarbageRevokeDriver(venue, IERC20(address(usd)), true)));
        halted();

        vm.prank(bob);
        (bool ok,) = address(station).call{gas: BIG_BLOCK}(abi.encodeWithSignature("stop()"));
        assertTrue(ok, "a pinned driver kept the halted station from stopping (undecodable revoke)");
        assertTrue(station.stopped());
    }

    function test_a_driver_whose_targets_return_undecodable_bytes_cannot_keep_the_station_from_stopping()
        public
    {
        deployed();
        pinnedDriverActsAs(address(new GarbageRevokeDriver(venue, IERC20(address(usd)), false)));
        halted();

        vm.prank(bob);
        (bool ok,) = address(station).call{gas: BIG_BLOCK}(abi.encodeWithSignature("stop()"));
        assertTrue(ok, "a pinned driver kept the halted station from stopping (undecodable targets)");
        assertTrue(station.stopped());
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

/// The local driver, except that its revoke names `count` calls to a gas
/// sink it lists among its targets.
contract ManyCallsRevokeDriver is IDriver {
    LocalDriver internal immutable inner;
    address internal immutable sink;
    uint256 internal immutable count;

    constructor(ILocalVenue venue_, IERC20 asset_, address sink_, uint256 count_) {
        inner = new LocalDriver(venue_, asset_);
        sink = sink_;
        count = count_;
    }

    function name() external pure returns (bytes32) {
        return "local";
    }

    function targets() external view returns (address[] memory list) {
        list = new address[](2);
        list[0] = inner.targets()[0];
        list[1] = sink;
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
        return inner.approveKey(station, key, until);
    }

    function revokeKey(address, address) external view returns (Call[] memory calls) {
        calls = new Call[](count);
        for (uint256 i = 0; i < count; i++) {
            calls[i] = Call(sink, 0, "");
        }
    }

    function deposit(address station, uint256 amount) external view returns (Call[] memory) {
        return inner.deposit(station, amount);
    }

    function withdraw(address station, uint256 amount) external view returns (Call[] memory) {
        return inner.withdraw(station, amount);
    }

    function close(
        address station,
        uint256 fraction,
        uint16 band
    ) external view returns (Call[] memory, bool) {
        return inner.close(station, fraction, band);
    }

    function sweep(address station, uint256 amount) external view returns (Call[] memory) {
        return inner.sweep(station, amount);
    }

    function valueOf(address station) external view returns (uint256, uint256) {
        return inner.valueOf(station);
    }
}

/// The local driver, except that either its `revokeKey` or its `targets`
/// view answers with one byte, which decodes as nothing.
contract GarbageRevokeDriver is IDriver {
    LocalDriver internal immutable inner;
    bool internal immutable inRevoke;

    constructor(ILocalVenue venue_, IERC20 asset_, bool inRevoke_) {
        inner = new LocalDriver(venue_, asset_);
        inRevoke = inRevoke_;
    }

    function _garbage() private pure {
        assembly {
            mstore(0, 0x01)
            return(0, 1)
        }
    }

    function name() external pure returns (bytes32) {
        return "local";
    }

    function targets() external view returns (address[] memory) {
        if (!inRevoke) _garbage();
        return inner.targets();
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
        return inner.approveKey(station, key, until);
    }

    function revokeKey(address station, address key) external view returns (Call[] memory) {
        if (inRevoke) _garbage();
        return inner.revokeKey(station, key);
    }

    function deposit(address station, uint256 amount) external view returns (Call[] memory) {
        return inner.deposit(station, amount);
    }

    function withdraw(address station, uint256 amount) external view returns (Call[] memory) {
        return inner.withdraw(station, amount);
    }

    function close(
        address station,
        uint256 fraction,
        uint16 band
    ) external view returns (Call[] memory, bool) {
        return inner.close(station, fraction, band);
    }

    function sweep(address station, uint256 amount) external view returns (Call[] memory) {
        return inner.sweep(station, amount);
    }

    function valueOf(address station) external view returns (uint256, uint256) {
        return inner.valueOf(station);
    }
}
