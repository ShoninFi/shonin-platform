// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {ILocalVenue, OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {IVault} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A driver whose revoke the venue refuses: the stand-in driver, with a
/// revoke that calls a venue function the venue refuses from a station.
contract RefusingRevokeDriver is IDriver {
    LocalDriver internal immutable inner;
    ILocalVenue internal immutable venue;

    constructor(ILocalVenue venue_, IERC20 asset_) {
        venue = venue_;
        inner = new LocalDriver(venue_, asset_);
    }

    function name() external view returns (bytes32) {
        return inner.name();
    }

    function targets() external view returns (address[] memory) {
        return inner.targets();
    }

    function restricted() external view returns (bytes32[] memory) {
        return inner.restricted();
    }

    function custody() external view returns (Custody) {
        return inner.custody();
    }

    function exitPath() external view returns (ExitPath) {
        return inner.exitPath();
    }

    function valuation() external view returns (ValuationKind) {
        return inner.valuation();
    }

    function approveKey(address s, address k, uint64 u) external view returns (Call[] memory) {
        return inner.approveKey(s, k, u);
    }

    function revokeKey(address, address) external view returns (Call[] memory calls) {
        calls = new Call[](1);
        // `listMarket` is operator-only: the venue refuses it from the station.
        calls[0] = Call(address(venue), 0, abi.encodeCall(ILocalVenue.listMarket, ("X", false, 1)));
    }

    function deposit(address s, uint256 a) external view returns (Call[] memory) {
        return inner.deposit(s, a);
    }

    function withdraw(address s, uint256 a) external view returns (Call[] memory) {
        return inner.withdraw(s, a);
    }

    function close(address s, uint256 f, uint16 b) external view returns (Call[] memory, bool) {
        return inner.close(s, f, b);
    }

    function sweep(address s, uint256 a) external view returns (Call[] memory) {
        return inner.sweep(s, a);
    }

    function valueOf(address s) external view returns (uint256, uint256) {
        return inner.valueOf(s);
    }
}

/// Properties of the registry's pinning edges, the key handover when a
/// venue refuses a revoke, the idle floor across pull/push cycles, and the
/// tank's cap across allowance changes and an old signed total.
contract RegistryStationTankPropertiesTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
        give(address(venue), 1_000_000e6);
    }

    /// A station created while a re-proposed, previously revoked
    /// implementation is ready but unsettled pins it; the admin's later
    /// revocation of the *old* current (which settles the slot) leaves the
    /// pinned one trading, and only revoking that exact implementation stops
    /// it.
    function test_r1_a_pin_taken_from_a_ready_unsettled_reproposal_survives_the_slot_settling() public {
        address a = address(driver);
        LocalDriver b = new LocalDriver(venue, IERC20(address(usd)));
        vm.startPrank(admin);
        attestation.proposeDriver("local", address(b));
        skip(TIMELOCK);
        attestation.revokeDriver("local", address(b)); // settles b as current, then revokes it
        assertEq(attestation.tradingDriver("local"), address(0));
        attestation.proposeDriver("local", address(b)); // re-proposed
        skip(TIMELOCK);
        vm.stopPrank();
        // Ready but unsettled: a new station pins b. (The service keys'
        // blessings lapsed over the two timelocks; they attest again.)
        assertEq(attestation.tradingDriver("local"), address(b));
        blessServiceKey(BUILDER_IMAGE, builder);
        blessServiceKey(REVIEWER_IMAGE, reviewer);
        (Vault fresh,) = openVault();
        Station freshStation = stationOf(fresh);
        assertEq(freshStation.pinnedDriver("local"), address(b));
        assertFalse(attestation.isDriverRevoked("local", address(b)));

        // The admin now revokes the long-gone `a`: that settles the slot
        // (b current, its old revocation cleared), and b keeps trading.
        vm.prank(admin);
        attestation.revokeDriver("local", a);
        assertFalse(attestation.isDriverRevoked("local", address(b)), "settling the slot re-revoked b");
        assertEq(attestation.tradingDriver("local"), address(b));
        address fresher = vm.addr(0xF2E5);
        blessExecutorKey(fresher, address(fresh), fresh.bundleHash());
        vm.prank(fresher);
        freshStation.approveKey("local", fresher);
        assertEq(freshStation.approvedKey("local"), fresher);

        // Only revoking b itself stops it.
        vm.prank(admin);
        attestation.revokeDriver("local", address(b));
        assertTrue(attestation.isDriverRevoked("local", address(b)));
        vm.prank(fresher);
        vm.expectRevert(Station.NotApproved.selector);
        freshStation.deposit("local", 0);
        // And a passer-by may now withdraw its key.
        vm.prank(bob);
        freshStation.revokeKey("local", fresher);
        assertEq(freshStation.approvedKey("local"), address(0));

        // The old station, pinned to `a`, is untouched throughout.
        assertEq(station.pinnedDriver("local"), a);
        assertTrue(attestation.isDriverRevoked("local", a));
    }

    /// A venue that refuses to withdraw a still-blessed key blocks a standby's
    /// strict handover, and a passer-by can't withdraw it either; once the
    /// key's blessing lapses the standby takes over best-effort, the skip is
    /// announced, and the old key stays on the venue only until its own
    /// validity there (its old blessing's end) runs out.
    function test_r2_a_venue_refusing_a_revoke_delays_a_handover_until_the_old_key_lapses() public {
        pinnedDriverActsAs(address(new RefusingRevokeDriver(venue, IERC20(address(usd)))));
        vm.prank(executor);
        station.approveKey("local", executor);
        uint64 oldUntil = venue.agentValidUntil(address(station), executor);
        assertEq(oldUntil, attestation.blessedUntil(executor, block.chainid, address(vault), bundle));

        address standby = vm.addr(0x5B);
        blessExecutorKey(standby, address(vault), bundle);
        vm.prank(standby);
        vm.expectRevert(); // the venue's refusal bubbles up: a strict revoke
        station.approveKey("local", standby);
        vm.prank(bob);
        vm.expectRevert(Station.StillBlessed.selector);
        station.revokeKey("local", executor);

        // The running key's blessing lapses; the standby re-attests.
        skip(BLESSING_TTL + 1);
        blessExecutorKey(standby, address(vault), bundle);
        vm.expectEmit(true, true, false, false, address(station));
        emit RevokeSkipped("local", executor);
        vm.prank(standby);
        station.approveKey("local", standby);
        assertEq(station.approvedKey("local"), standby);
        // The old key is still approved at the venue by its own record...
        assertEq(venue.agentValidUntil(address(station), executor), oldUntil);
        // ...but that validity is already in the past, so the venue refuses it.
        assertFalse(venue.isAgent(address(station), executor));
        // A strict withdrawal is still refused by this venue: documented limit.
        vm.prank(bob);
        vm.expectRevert();
        station.revokeKey("local", executor);
    }

    event RevokeSkipped(bytes32 indexed driver, address indexed key);

    /// Pull to the floor, push back, pull again, lose at the venue, pull
    /// again: the cash left in the vault is never below the floor of the
    /// value the last valuation and the pulls since it add up to, and after
    /// a valuation reflects a loss the floor follows the smaller value.
    function test_r3_the_idle_floor_holds_across_pull_push_cycles_and_a_loss() public {
        depositAs(vault, alice, 100_000e6);
        uint256 total = vault.cash(); // 100k + dead
        uint256 floor = (total + 1) * 500 / 10_000 + 1;
        vm.startPrank(executor);
        vm.expectRevert(IVault.BelowFloor.selector);
        station.pull(total - floor + 1);
        station.pull(total - floor);
        assertEq(vault.cash(), floor);
        // Push it all back: the stale station figure plus the cash now
        // overstates the value, so the floor is only higher.
        station.push(total - floor);
        vm.expectRevert(IVault.BelowFloor.selector);
        station.pull(total - floor);
        vm.stopPrank();
        // A valuation resets the picture; pull to the floor again.
        value(vault);
        vm.prank(executor);
        station.pull(total - floor);
        assertEq(vault.cash(), floor);

        // The station loses 40% at the venue (a bad trade): the next
        // valuation shrinks the value, and nothing more can be pulled.
        vm.startPrank(executor);
        station.approveKey("local", executor);
        station.deposit("local", 90_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        setPrice(SPOT, PRICE * 60 / 100, 0);
        value(vault);
        uint256 valueNow = vault.totalValue();
        assertLt(valueNow, 80_000e6);
        // The floor is now 5% of the shrunken value (plus the virtual unit):
        // the cash may be pulled down to it and not a unit below.
        uint256 floorAfter = ((valueNow + 1) * 500 + 9999) / 10_000;
        uint256 room = vault.cash() - floorAfter;
        vm.startPrank(executor);
        vm.expectRevert(IVault.BelowFloor.selector);
        station.pull(room + 1);
        station.pull(room);
        vm.stopPrank();
        assertEq(vault.cash(), floorAfter);
        assertGe(vault.cash() * 10_000, valueNow * 500, "the cash fell below the floor of the new value");
    }

    /// Two allowance changes and an old signed total between them never let
    /// the tank be charged more than the allowances in force over the time
    /// they were in force, and never twice for one interval.
    function test_r4_the_tank_cap_across_allowance_changes_and_an_old_total_never_double_counts() public {
        // 2 dollars a day from activation.
        uint64 t0 = uint64(vm.getBlockTimestamp());
        give(author, 1000e6);
        vm.startPrank(author);
        usd.approve(address(vault), 1000e6);
        vault.fundTank(1000e6);
        vm.stopPrank();
        skip(2 days);
        // The executor signs a total as of now (t0 + 2d), but it isn't sent yet.
        uint64 asOf1 = uint64(vm.getBlockTimestamp());
        bytes memory sig1 = usage(vault, 1000e6, asOf1);
        skip(1 days);
        // The author raises the allowances to 10 dollars a day at t0 + 3d.
        vm.prank(author);
        vault.setAllowances(allowancesCosting(10e6));
        skip(1 days);
        // At t0 + 4d the old total lands (the executor has attested again
        // meanwhile, as it does every day): the cap is 2/day * 3d (carried),
        // nothing for the day after the change since the total predates it.
        blessExecutorKey(executor, address(vault), bundle);
        uint256 before = usd.balanceOf(platform);
        vault.keep(1000e6, asOf1, sig1);
        uint256 charged1 = usd.balanceOf(platform) - before;
        assertEq(charged1, 6e6, "the old total was charged beyond the old allowances' time");
        // A fresh total as of now: the cap is only the day at 10/day since
        // the change (t0 + 3d to t0 + 4d); the interval before was covered.
        uint64 asOf2 = uint64(vm.getBlockTimestamp());
        before = usd.balanceOf(platform);
        vault.keep(2000e6, asOf2, usage(vault, 2000e6, asOf2));
        uint256 charged2 = usd.balanceOf(platform) - before;
        assertEq(charged2, 10e6, "the interval after the change was charged other than at the new rate");
        // In all: 2/day for three days plus 10/day for one: never more.
        assertEq(charged1 + charged2, 16e6);
        t0;
    }
}
