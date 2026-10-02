// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {Vault} from "../../src/Vault.sol";
import {OrderRequest} from "../../src/interfaces/ILocalVenue.sol";
import {VaultState} from "../../src/interfaces/IVault.sol";
import {Fixture} from "../Fixture.sol";
import {Deps, VaultHandler} from "./VaultHandler.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";

/// The vault's invariants over long random walks: deposits, withdrawals,
/// valuations, partial settles, the executor's pulls, pushes and trades,
/// market moves inside and outside the band, claims, both fallbacks, units,
/// the tank and its bills, allowances, the idle floor, retiring, freezing,
/// killing, stopping, closing, paying out a closed vault, paydays, and every
/// call the admin key can make.
///
/// CI runs a short campaign per invariant, set below. Locally each was also
/// run at 256 runs of 500 calls, by raising the two settings below.
///
/// forge-config: default.invariant.runs = 64
/// forge-config: default.invariant.depth = 250
contract VaultInvariantsTest is Fixture {
    address internal constant DEAD = 0x000000000000000000000000000000000000dEaD;

    Vault internal vault;
    VaultHandler internal handler;

    function setUp() public override {
        super.setUp();
        (vault,) = openVault();
        handler = new VaultHandler(
            Deps({
                vault: vault,
                usd: usd,
                venue: venue,
                attestation: attestation,
                prices: prices,
                factory: factory,
                driver: driver,
                admin: admin,
                platform: platform,
                operator: operator,
                author: author,
                authorKey: authorKey,
                executorKey: EXECUTOR_KEY,
                attestorKey: ATTESTOR_KEY,
                executorImage: EXECUTOR_IMAGE,
                project: PROJECT
            })
        );
        handler.bootstrap();

        bytes4[] memory actions = new bytes4[](14);
        actions[0] = VaultHandler.requestDeposit.selector;
        actions[1] = VaultHandler.requestRedeem.selector;
        actions[2] = VaultHandler.recordValuation.selector;
        actions[3] = VaultHandler.settle.selector;
        actions[4] = VaultHandler.miscAct.selector;
        actions[5] = VaultHandler.warp.selector;
        actions[6] = VaultHandler.movePrice.selector;
        actions[7] = VaultHandler.adminAct.selector;
        actions[8] = VaultHandler.executorAct.selector;
        actions[9] = VaultHandler.claimAct.selector;
        actions[10] = VaultHandler.tankAct.selector;
        actions[11] = VaultHandler.lifecycleAct.selector;
        actions[12] = VaultHandler.closedAct.selector;
        actions[13] = VaultHandler.experimentAct.selector;
        targetContract(address(handler));
        targetSelector(StdInvariant.FuzzSelector({addr: address(handler), selectors: actions}));
    }

    // 1. Solvency --------------------------------------------------------------

    /// The vault's dollar balance covers its cash, the tank, the escrow, the
    /// unit reserve and every payout held for a receiver who refused it; and
    /// the unit reserve covers everything its holders can collect.
    function invariant_1_solvency_the_balance_covers_every_account() public view {
        assertGe(usd.balanceOf(address(vault)), handler.owed(), "the vault holds less than it owes");
        (, uint256 reserve) = vault.units();
        assertGe(reserve, handler.unitClaims(), "the unit reserve is less than its holders can collect");
    }

    /// At any moment everyone can be let out at the exit side: halting,
    /// unwinding and closing goes through, every request, claim, holder and
    /// unit holder is paid, the vault ends owing nothing but its tank and
    /// the dead shares' part, and the payouts come to no more than the cash
    /// and the stations at exit held.
    function invariant_1_solvency_cash_and_stations_at_exit_pay_every_share() public view {
        assertEq(handler.payoutBreaks(), 0, handler.lastBreak());
    }

    // 2. No one else pays for a move --------------------------------------------

    /// A holder who never acts never sees their net price per share (mark
    /// side, after the fee owed) fall for anyone else's step while the
    /// market stands still, beyond rounding and the holding fee the step
    /// synced.
    function invariant_2_a_passive_holder_never_pays_for_anyone_elses_move() public view {
        assertEq(handler.passiveBreaks(), 0, handler.lastBreak());
    }

    // 3. Rounding favours the pool ---------------------------------------------

    function invariant_3_a_round_trip_never_returns_more_than_went_in() public view {
        assertEq(handler.roundTripBreaks(), 0, handler.lastBreak());
    }

    // 4. Fees never exceed what's owed -------------------------------------------

    /// A payday mints shares worth at most the profit fee owed on the value
    /// it is paid on and hands over exactly the holding fee accrued; the
    /// holding fee minted is at most its rate for the time elapsed; a
    /// leaver's fee slice is at most their shares' part of the fee owed.
    function invariant_4_fees_never_exceed_what_is_owed() public view {
        assertEq(handler.feeBreaks(), 0, handler.lastBreak());
    }

    function invariant_4_the_mark_never_falls() public view {
        assertEq(handler.markBreaks(), 0, handler.lastBreak());
    }

    // 5. Fees owed can shrink ----------------------------------------------------

    /// A move shown at one valuation and given back before payday mints
    /// nothing: payday mints what it would have had the market never moved.
    function invariant_5_a_gain_given_back_before_payday_mints_nothing() public view {
        assertEq(handler.givenBackBreaks(), 0, handler.lastBreak());
    }

    // 6. The tank never owes -----------------------------------------------------

    /// The tank's balance is what came in less what was charged and taken
    /// out, so it never pays more than it holds; no bill charges more than
    /// the allowances cost for the time it covers, nor all bills together
    /// more than the allowances cost since activation; the usage total never
    /// goes back.
    function invariant_6_the_tank_never_owes() public view {
        assertEq(handler.tankBreaks(), 0, handler.lastBreak());
        assertEq(vault.tank().balance, handler.tankBooks(), "the tank's balance is not its books");
        assertLe(handler.tankCharged(), handler.capSoFar() + 64, "the tank was charged above its allowances");
    }

    // 7. Money only moves between the vault's own accounts ----------------------

    /// Dollars leave the vault only for its station or someone it may pay
    /// (a depositor, the author, the platform), the station's only for the
    /// vault or its venue account, the venue's only for the station.
    function invariant_7_money_only_moves_between_the_vaults_own_accounts() public view {
        assertEq(handler.moneyBreaks(), 0, handler.lastBreak());
    }

    /// No pull by the home station leaves the cash under the idle floor in
    /// force, measured on the value at the last valuation.
    function invariant_7_the_home_station_never_pulls_below_the_idle_floor() public view {
        assertEq(handler.floorBreaks(), 0, handler.lastBreak());
    }

    // 8. Every holder signed -------------------------------------------------------

    function invariant_8_every_holder_signed() public view {
        uint256 count = handler.holderCount();
        for (uint256 i = 0; i < count; i++) {
            address h = handler.holders(i);
            if (h == platform || h == address(vault) || h == DEAD) continue;
            if (vault.balanceOf(h) == 0) continue;
            assertTrue(vault.statementOf(h) != bytes32(0), "a holder never signed the statement");
        }
    }

    // 12. Access control -----------------------------------------------------------

    /// The admin key can propose, revoke and kill, and none of it moves a
    /// dollar or changes a book; every other money-moving call it tries is
    /// refused. The station only stops once the halt's grace has passed.
    function invariant_12_the_admin_key_moves_no_money() public view {
        assertEq(handler.adminBreaks(), 0, handler.lastBreak());
        assertEq(handler.graceBreaks(), 0, handler.lastBreak());
    }

    /// What the walk reached, with `-vv`.
    function afterInvariant() public {
        uint256 count = handler.hitCount();
        for (uint256 i = 0; i < count; i++) {
            bytes32 name = handler.hitNames(i);
            emit log_named_uint(string(abi.encodePacked(name)), handler.hits(name));
        }
    }
}

/// Two states the walk reaches, pinned down as plain tests.
contract VaultInvariantCasesTest is Fixture {
    Vault internal vault;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault,) = openVault();
        station = stationOf(vault);
        give(address(venue), 100_000_000e6);
        vm.prank(executor);
        station.approveKey("local", executor);
    }

    /// Spot bought outright and a perp bought on margin beside it: once the
    /// vault is halted and its station stopped, anyone can unwind all of it,
    /// so the vault can close and pay everyone.
    function test_a_stopped_station_holding_spot_beside_a_levered_perp_unwinds_in_full() public {
        depositAs(vault, alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(90_000e6);
        station.deposit("local", 90_000e6);
        OrderRequest[] memory orders = new OrderRequest[](2);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        orders[1] =
            OrderRequest({market: PERP, isBuy: true, size: 2.5e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        assertLt(venue.cashOf(address(station)), 0, "the perp is bought on margin");

        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE + 1);
        station.stop();
        station.unwind(1e18);
        value(vault);
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);
    }

    /// Three holders take the fallback and the station sends money home in
    /// uneven amounts: the unit reserve always covers what they can collect,
    /// and each of them can collect it.
    function testFuzz_every_unit_holder_can_collect_what_came_home_for_them(uint256 seed) public {
        address[3] memory who = [alice, bob, author];
        for (uint256 i = 0; i < 3; i++) {
            depositAs(vault, who[i], 50_000e6 + uint256(keccak256(abi.encode(seed, i))) % 100_000e6);
        }
        uint256 out = vault.cash() * 9 / 10;
        vm.prank(executor);
        station.pull(out);
        for (uint256 i = 0; i < 3; i++) {
            uint256 shares = vault.balanceOf(who[i]) / (2 + i);
            vm.prank(who[i]);
            vault.requestRedeem(shares, who[i]);
        }
        // Nothing values for a day: the leavers take the fallback one by
        // one, with money coming home between them.
        skip(1 days + SLOT);
        blessExecutorKey(executor, address(vault), vault.bundleHash());
        for (uint256 i = 0; i < 3; i++) {
            uint256 id = vault.requestCount() - 2 + i;
            vm.prank(who[i]);
            vault.takeProRata(id);
            for (uint256 k = 0; k < 3; k++) {
                uint256 idle = usd.balanceOf(address(station));
                uint256 amount = 1 + uint256(keccak256(abi.encode(seed, i, k))) % (idle / 8);
                vm.prank(executor);
                station.push(amount);
                uint256 claims;
                for (uint256 j = 0; j < 3; j++) {
                    (, uint256 claimable) = vault.unitsOf(who[j]);
                    claims += claimable;
                }
                (, uint256 reserve) = vault.units();
                assertGe(reserve, claims, "the unit reserve is less than its holders can collect");
            }
        }
        for (uint256 i = 0; i < 3; i++) {
            vm.prank(who[i]);
            vault.collectUnits();
        }
    }
}
