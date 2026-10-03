// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {ClaimBase} from "./ClaimBase.sol";
import {TakingDriver} from "./invariant/VaultHandler.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A running vault keeps the driver it was created with. Whatever the admin
/// key approves later, even once the registry's timelock has passed, reaches
/// only stations created afterwards; revoking is the admin's one power over
/// a running station's driver, and it stops trading without stopping exits.
contract AdminChangesWaitForTheTimelockTest is ClaimBase {
    function setUp() public override {
        super.setUp();
        vm.prank(executor);
        station.approveKey("local", executor);
    }

    function test_a_driver_approved_later_never_reaches_a_running_vault() public {
        depositAs(vault, alice, 100_000e6);
        deploySpot(90_000e6, 60_000e6, 0.5e18);
        TakingDriver replacement = new TakingDriver(IERC20(address(usd)), admin);
        vm.prank(admin);
        attestation.proposeDriver("local", address(replacement));
        skip(TIMELOCK * 10);
        assertEq(attestation.tradingDriver("local"), address(replacement), "the replacement was not approved");

        blessExecutorKey(executor, address(vault), bundle);
        uint256 held = usd.balanceOf(address(station));
        vm.prank(executor);
        station.deposit("local", 1000e6);
        assertEq(station.pinnedDriver("local"), address(driver), "the station's driver changed");
        assertEq(usd.balanceOf(admin), 0, "the replacement reached a running vault");
        assertEq(usd.balanceOf(address(station)), held - 1000e6, "the deposit didn't go to the venue");

        // A full unwind runs through the pinned driver too.
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);
        assertEq(usd.balanceOf(admin), 0, "the replacement reached a stopped station");
    }

    function test_a_bridge_approved_later_never_reaches_a_running_vault() public {
        address replacement = makeAddr("replacement bridge");
        bytes32[] memory bridges = station.spec().bridges;
        if (bridges.length == 0) return;
        address before = station.pinnedBridge(bridges[0]);
        vm.prank(admin);
        attestation.proposeBridge(bridges[0], replacement);
        skip(TIMELOCK * 10);
        assertEq(attestation.bridgeOf(bridges[0]), replacement);
        assertEq(station.pinnedBridge(bridges[0]), before, "the station's bridge changed");
    }

    function test_a_station_created_after_an_approval_pins_it() public {
        TakingDriver replacement = new TakingDriver(IERC20(address(usd)), admin);
        vm.prank(admin);
        attestation.proposeDriver("local", address(replacement));
        skip(TIMELOCK);
        (Vault later,) = openVault();
        Station fresh = stationOf(later);
        assertEq(fresh.pinnedDriver("local"), address(replacement));
        assertEq(station.pinnedDriver("local"), address(driver), "the running vault's driver changed");
    }

    function test_revoking_a_pinned_driver_stops_trading_and_leaves_exits_working() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        uint256 cash = vault.cash();
        deploySpot(cash * 94 / 100, cash * 94 / 100, (cash * 94 / 100) * 9 / 10 * 1e18 / PRICE);
        vm.prank(admin);
        attestation.revokeDriver("local", address(driver));

        vm.prank(executor);
        vm.expectRevert(Station.NotApproved.selector);
        station.deposit("local", 1);

        uint256 fair = worth(bobShares);
        uint256 id = claimFor(bob, bobShares);
        vault.settleClaim(id);
        // Less only the transaction fee and the profit fee the shares owe.
        assertGe(usd.balanceOf(bob), fair * 9980 / 10_000, "the leaver was paid short");
    }
}
