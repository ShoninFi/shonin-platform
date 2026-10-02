// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {IStationVault} from "../src/interfaces/IStation.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// Nothing a driver prepares reaches the vault, the station itself, its
/// factory or the registry, even a driver that lists them among its targets:
/// a station's driver can never touch the vault's cash.
contract DriverCannotReachTheVaultTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function test_a_driver_that_lists_the_vault_as_a_target_cannot_reach_its_cash() public {
        depositAs(vault, alice, 100_000e6);
        // One more valuation so the floor is measured on her money too.
        value(vault);
        uint256 cashBefore = vault.cash();
        assertApproxEqAbs(cashBefore, 100_000e6, 1e6);

        // The vault's own driver turns out hostile.
        address taker = address(new Taker());
        HostileDriver hostile = new HostileDriver(IERC20(address(usd)), vault, taker);
        pinnedDriverActsAs(address(hostile));

        // A stranger "revokes" an address that was never a key: the station
        // refuses the driver's call to the vault.
        vm.prank(bob);
        vm.expectRevert(Station.BadCall.selector);
        station.revokeKey("local", makeAddr("nobody"));

        assertEq(usd.balanceOf(taker), 0, "money left through a driver");
        assertEq(vault.cash(), cashBefore, "the vault's cash moved");
    }
}

/// A driver whose every prepared call list calls the vault, approves an
/// address and lets it take from the station; it lists the vault among its
/// targets.
contract HostileDriver is IDriver {
    IERC20 internal immutable dollar;
    Vault internal immutable vault;
    address internal immutable taker;

    constructor(IERC20 dollar_, Vault vault_, address taker_) {
        dollar = dollar_;
        vault = vault_;
        taker = taker_;
    }

    function name() external pure returns (bytes32) {
        return "local";
    }

    function targets() external view returns (address[] memory list) {
        list = new address[](2);
        list[0] = address(vault);
        list[1] = taker;
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

    /// Everything the idle floor lets through, by the vault's own arithmetic
    /// when nothing has been sent out and the station was empty at the last
    /// valuation: value is the cash plus one.
    function _amount() internal view returns (uint256) {
        uint256 cash = vault.cash();
        (uint16 floorBps,,) = vault.idleFloor();
        uint256 floor = ((cash + 1) * floorBps + 9999) / 10_000;
        return cash > floor ? cash - floor : 0;
    }

    function _prepare(address station) internal view returns (Call[] memory calls) {
        uint256 amount = _amount();
        calls = new Call[](3);
        calls[0] = Call(address(vault), 0, abi.encodeCall(IStationVault.toStation, (amount)));
        calls[1] = Call(address(dollar), 0, abi.encodeCall(IERC20.approve, (taker, amount)));
        calls[2] = Call(taker, 0, abi.encodeCall(Taker.take, (dollar, station, amount)));
    }

    function approveKey(address station, address, uint64) external view returns (Call[] memory) {
        return _prepare(station);
    }

    function revokeKey(address station, address) external view returns (Call[] memory) {
        return _prepare(station);
    }

    function deposit(address station, uint256) external view returns (Call[] memory) {
        return _prepare(station);
    }

    function withdraw(address station, uint256) external view returns (Call[] memory) {
        return _prepare(station);
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

/// Where the money ends up.
contract Taker {
    function take(IERC20 dollar, address from, uint256 amount) external {
        dollar.transferFrom(from, address(this), amount);
    }
}
