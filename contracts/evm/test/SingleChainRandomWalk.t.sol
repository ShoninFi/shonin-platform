// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {IVault, Request, RequestKind, VaultState} from "../src/interfaces/IVault.sol";
import {VaultConfig} from "../src/interfaces/IVaultFactory.sol";
import {Fixture} from "./Fixture.sol";

/// Long random walks over a single-chain vault (deposits,
/// redemptions, partial settles, valuations, market moves inside and outside
/// the band, the executor's pulls, pushes and trades, claims, the fallbacks,
/// tank fills, serves and paydays), checking after every step that:
/// - the vault holds at least what it owes (cash, tank, escrow, unit reserve);
/// - `settle` never reverts and always lets the next valuation open;
/// - a holder who never acts never loses net worth per share (mark side,
///   after the fee owed) to anyone else's move, while the market stands still.
/// The holding fee is off so a stayer's price only moves for a reason.
contract SingleChainRandomWalkTest is Fixture {
    address internal constant DEAD = 0x000000000000000000000000000000000000dEaD;
    Vault internal vault;
    Station internal station;
    address internal carol;
    uint256 internal carolKey;
    address internal dave;
    uint256 internal daveKey;
    address[4] internal actors;

    /// The market hasn't moved since the last valuation priced: a settle
    /// meets the pool as the snapshot found it.
    bool internal still;
    uint256 internal spot;

    function setUp() public override {
        super.setUp();
        (carol, carolKey) = makeAddrAndKey("carol");
        (dave, daveKey) = makeAddrAndKey("dave");
        VaultConfig memory config = factory.config();
        config.holdingBps = 0;
        vm.prank(admin);
        factory.setConfig(config);
        (vault,) = openVault();
        station = stationOf(vault);
        give(address(venue), 100_000_000e6);
        actors = [alice, bob, carol, author];
        vm.prank(executor);
        station.approveKey("local", executor);
        spot = PRICE;
    }

    function keyFor(address who) internal view returns (uint256) {
        if (who == carol) return carolKey;
        if (who == dave) return daveKey;
        return keyOf(who);
    }

    function depositFor(address who, uint256 amount) internal {
        bytes memory statement = statementBy(keyFor(who), address(vault), who, vault.restrictedText());
        give(who, amount);
        vm.startPrank(who);
        usd.approve(address(vault), amount);
        vault.requestDeposit(amount, who, statement);
        vm.stopPrank();
    }

    function netPerShare() internal view returns (uint256) {
        (uint256 markValue,) = station.value();
        (uint256 units,) = vault.units();
        uint256 total = vault.cash() + markValue * (1e18 - units) / 1e18 + 1;
        uint256 supply = vault.totalSupply() + 1e12;
        uint256 basis = (vault.mark() * supply + 1e30 - 1) / 1e30;
        uint256 fee = total > basis ? (total - basis) * 1500 / 10_000 : 0;
        return (total - fee) * 1e36 / supply;
    }

    function owedByVault() internal view returns (uint256) {
        (, uint256 reserve) = vault.units();
        return vault.cash() + vault.tank().balance + vault.escrow() + reserve;
    }

    function checkSolvent(string memory where) internal view {
        assertGe(usd.balanceOf(address(vault)), owedByVault(), where);
    }

    function nextSlot() internal {
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
    }

    function rnd(uint256 seed, uint256 i, uint256 salt) internal pure returns (uint256) {
        return uint256(keccak256(abi.encode(seed, i, salt)));
    }

    function settleAll() internal {
        // The queue can always be settled.
        try vault.settle(type(uint256).max) {}
        catch (bytes memory reason) {
            emit log_named_bytes("settle reverted", reason);
            fail();
        }
    }

    function valueNow() internal {
        nextSlot();
        settleAll();
        uint256 before = netPerShare();
        try vault.recordValuation() {
            still = true;
            // A valuation (and a payday at it) moves nobody's price.
            uint256 afterwards = netPerShare();
            assertGe(afterwards + afterwards / 1e9 + 1e12, before, "a valuation lowered a stayer's price");
        } catch (bytes memory reason) {
            // Nothing may hold a single-chain valuation off once the queue
            // is settled, except a state that has stopped valuing.
            VaultState state = vault.state();
            if (state != VaultState.Closed) {
                emit log_named_bytes("recordValuation reverted", reason);
                fail();
            }
        }
    }

    /// A walk that serves a unit holder: its rounding is the reason for the
    /// relative tolerance below.
    function test_a_walk_that_serves_units() public {
        testFuzz_random_walk(71_246_414_430_136_455_837_760_236_877_438_172_796_722);
    }

    function testFuzz_random_walk(uint256 seed) public {
        depositFor(dave, 200_000e6);
        depositFor(alice, 100_000e6);
        depositFor(bob, 100_000e6);
        depositFor(author, 50_000e6);
        valueNow();
        settleAll();

        for (uint256 i = 0; i < 60; i++) {
            uint256 op = rnd(seed, i, 0) % 16;
            uint256 a = rnd(seed, i, 1);
            uint256 b = rnd(seed, i, 2);
            address who = actors[a % 4];
            uint256 before = netPerShare();
            bool check;

            if (op == 0) {
                if (vault.state() == VaultState.Open) depositFor(who, 10e6 + b % 150_000e6);
            } else if (op == 1) {
                uint256 held = vault.balanceOf(who);
                if (held != 0 && vault.state() != VaultState.Closed) {
                    vm.prank(who);
                    try vault.requestRedeem(held * (1 + b % 100) / 100, who) {} catch {}
                }
            } else if (op == 2 || op == 15) {
                valueNow();
            } else if (op == 3) {
                if (still) check = true;
                try vault.settle(1 + b % 3) {}
                catch (bytes memory reason) {
                    emit log_named_bytes("partial settle reverted", reason);
                    fail();
                }
            } else if (op == 4) {
                // Up or down by up to 8%, the bid up to 1.5% under the
                // venue's price (sometimes outside the 1% band).
                uint256 moved = spot * (9200 + a % 1601) / 10_000;
                uint256 under = b % 151;
                vm.prank(operator);
                venue.setPrices(
                    SPOT, moved, moved, moved * (10_000 - under) / 10_000, moved * (10_000 + under) / 10_000
                );
                spot = moved;
                still = false;
            } else if (op == 5) {
                uint256 amount = vault.cash() * (a % 60) / 100;
                vm.startPrank(executor);
                try station.pull(amount) {
                    if (amount != 0) {
                        station.deposit("local", amount);
                        OrderRequest[] memory orders = new OrderRequest[](1);
                        orders[0] = OrderRequest({
                            market: SPOT,
                            isBuy: true,
                            size: amount * 1e18 / (spot * 102 / 100),
                            limitPrice: type(uint256).max,
                            reduceOnly: false
                        });
                        try venue.place(address(station), orders) {} catch {}
                    }
                } catch {}
                vm.stopPrank();
                still = false;
            } else if (op == 6) {
                // Sell some spot and push the station's idle home: at par.
                int256 size = venue.positionOf(address(station), SPOT).size;
                vm.startPrank(executor);
                if (size > 0 && a % 2 == 0) {
                    OrderRequest[] memory orders = new OrderRequest[](1);
                    orders[0] = OrderRequest({
                        market: SPOT,
                        isBuy: false,
                        size: uint256(size) * (1 + b % 100) / 100,
                        limitPrice: 0,
                        reduceOnly: true
                    });
                    try venue.place(address(station), orders) {} catch {}
                    int256 cash = venue.cashOf(address(station));
                    if (cash > 0) try station.withdraw("local", uint256(cash)) {} catch {}
                    still = false;
                } else {
                    check = true;
                }
                uint256 idle = usd.balanceOf(address(station));
                if (idle != 0 && !station.stopped()) try station.push(idle) {} catch {}
                vm.stopPrank();
            } else if (op == 7) {
                uint256 claims = vault.claimCount();
                if (claims != 0) {
                    check = true;
                    try vault.settleClaim(1 + a % claims) {} catch {}
                }
            } else if (op == 8) {
                skip(a % 2 == 0 ? 1 days + SLOT : 3 days);
                // The executor renews its blessing and its venue approval.
                blessExecutorKey(executor, address(vault), vault.bundleHash());
                if (!station.stopped()) {
                    vm.prank(executor);
                    station.approveKey("local", executor);
                }
            } else if (op == 9 || op == 10) {
                uint256 count = vault.requestCount();
                if (count != 0) {
                    uint256 id = 1 + a % count;
                    Request memory r = vault.requestAt(id);
                    check = true;
                    vm.prank(r.owner);
                    if (op == 9) {
                        try vault.takeProRata(id) {} catch {}
                    } else {
                        try vault.claimInstead(id) {} catch {}
                    }
                }
            } else if (op == 11) {
                uint256 claims = vault.claimCount();
                if (claims != 0) {
                    uint256 id = 1 + a % claims;
                    check = true;
                    vm.prank(vault.claimAt(id).owner);
                    try vault.takeProRataClaim(id) {} catch {}
                }
            } else if (op == 12 && b % 7 == 0) {
                // The author retires between a valuation and its settlement,
                // or the admin kills: the slot's deposits go back.
                check = true;
                if (vault.state() == VaultState.Open) {
                    vm.prank(author);
                    vault.retire();
                } else if (
                    b % 2 == 0 && vault.state() != VaultState.Killed && vault.state() != VaultState.Closed
                ) {
                    vm.prank(admin);
                    vault.kill();
                }
            } else if (op == 12) {
                uint256 held = vault.balanceOf(author);
                if (held != 0) {
                    vm.prank(author);
                    try vault.topUpFromShares(held * (1 + b % 50) / 100) {} catch {}
                }
            } else if (op == 13) {
                check = true;
                try vault.serveProRata(who) {} catch {}
                try vault.serveProRata(dave) {} catch {}
            } else if (op == 14) {
                uint256 count = vault.requestCount();
                if (count != 0) {
                    vm.prank(vault.requestAt(1 + a % count).owner);
                    try vault.collectUnits() {} catch {}
                }
            }

            checkSolvent("insolvent after a step");
            if (check) {
                uint256 afterwards = netPerShare();
                if (afterwards + afterwards / 1e9 + 1e12 < before) {
                    emit log_named_uint("step", i);
                    emit log_named_uint("op", op);
                    emit log_named_uint("price before", before);
                    emit log_named_uint("price after ", afterwards);
                    fail();
                }
            }
        }
        (uint256 liveUnits,) = vault.units();
        emit log_named_uint("claims booked", vault.claimCount());
        emit log_named_uint("requests", vault.requestCount());
        emit log_named_uint("valuations", vault.valuationCount());
        emit log_named_uint("units outstanding", liveUnits);
        // Wind it all the way down: every valuation still opens, and the
        // vault closes.
        settleAll();
        valueNow();
        settleAll();
        windDown(seed);
    }

    /// Kill, unwind at a calm market, close, and let everyone out: nothing
    /// is stuck and the vault ends holding only what it owes.
    function windDown(uint256 seed) internal {
        if (vault.state() != VaultState.Killed && vault.state() != VaultState.Closed) {
            vm.prank(admin);
            vault.kill();
        }
        vm.prank(operator);
        venue.setPrices(SPOT, spot, spot, spot, spot);
        skip(HALT_GRACE + 1);
        if (!station.stopped()) station.stop();
        (uint256 m,) = station.value();
        if (m != 0) station.unwind(1e18);
        valueNow();
        settleAll();
        // A request made after the last valuation doesn't hold the close.
        vault.close();
        checkSolvent("insolvent at the close");
        uint256 count = vault.requestCount();
        for (uint256 id = 1; id <= count; id++) {
            Request memory r = vault.requestAt(id);
            if (!r.done && r.kind != RequestKind.Claim) vault.settleClosed(id);
        }
        uint256 claims = vault.claimCount();
        for (uint256 id = 1; id <= claims; id++) {
            try vault.claimClosed(id) {} catch {}
        }
        address[6] memory everyone = [alice, bob, carol, author, dave, platform];
        for (uint256 k = 0; k < everyone.length; k++) {
            address who = everyone[k];
            uint256 held = vault.balanceOf(who);
            if (held != 0) {
                vm.prank(who);
                vault.redeemClosed(held, who);
            }
            (uint256 raw,) = vault.unitsOf(who);
            if (raw != 0) {
                try vault.serveProRata(who) {}
                catch (bytes memory reason) {
                    emit log_named_bytes("a unit holder couldn't be served after the close", reason);
                    fail();
                }
            }
            (, uint256 claimable) = vault.unitsOf(who);
            if (claimable != 0) {
                vm.prank(who);
                vault.collectUnits();
            }
            checkSolvent("insolvent letting everyone out");
        }
        // What is left: the dead shares' part, the tank, and rounding.
        (, uint256 reserve) = vault.units();
        emit log_named_uint("cash left", vault.cash());
        emit log_named_uint("unit reserve left", reserve);
        emit log_named_uint("escrow left", vault.escrow());
        assertEq(vault.escrow(), 0, "escrow left behind");
        assertLe(reserve, 1e3, "unit reserve left behind");
        assertEq(vault.totalSupply(), vault.balanceOf(DEAD), "shares left that nobody can redeem");
        seed;
    }
}
