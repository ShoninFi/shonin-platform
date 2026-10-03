// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {Vault} from "../../src/Vault.sol";
import {OrderRequest} from "../../src/interfaces/ILocalVenue.sol";
import {
    Claim,
    ClaimState,
    Request,
    RequestKind,
    Valuation,
    ValuationStatus
} from "../../src/interfaces/IVault.sol";
import {Fixture} from "../Fixture.sol";

/// A bank run on a single-chain vault. Between three and twelve holders and
/// the fee recipients all leave, in random order, at random prices inside
/// the exit band with moments outside it, with a book that is levered or
/// not and cash that runs out partway. Claims are settled by a stranger, as
/// anyone may; some leavers wait out a day and take the fallback.
///
/// Every payout is no more than the leaver's fraction of the vault at the
/// exit side; the author's and the platform's fee shares leave through the
/// same door at the same price and nobody who stays pays for any exit; a
/// claim's close sells only the leaver's fraction of each position; the tank
/// is never touched; and the vault ends owing nothing: every request done,
/// every claim paid or taken through the fallback, every unit served and
/// collected, and a final close pays out what is left.
contract BankRunTest is Fixture {
    address internal constant DEAD = 0x000000000000000000000000000000000000dEaD;
    uint256 internal constant WAD = 1e18;
    uint256 internal constant OFFSET = 1e12;

    Vault internal vault;
    Station internal station;
    address internal stranger = makeAddr("stranger");

    address[] internal leavers;
    mapping(address => uint256) internal keyFor;
    mapping(address => bool) internal asked;
    uint256 internal seed;
    uint256 internal step;
    uint256 internal tankAtStart;
    uint256 internal base;
    bool internal wide;
    /// What the run went through, logged with `-vv`.
    uint256 internal settledExits;
    uint256 internal closedClaims;
    uint256 internal fallbacks;
    uint256 internal wideRounds;

    function setUp() public override {
        super.setUp();
        (vault,) = openVault();
        station = stationOf(vault);
        // The venue is every trade's counterparty: reserves for the gains.
        give(address(venue), 100_000_000e6);
        vm.prank(executor);
        station.approveKey("local", executor);
    }

    /// The withdrawals settled in the current slot: shares and what each was
    /// paid, to compare their prices.
    uint256[] internal slotShares;
    uint256[] internal slotPaid;

    function rnd() internal returns (uint256) {
        return uint256(keccak256(abi.encode(seed, step++)));
    }

    /// Locally also run at 1000 runs, by raising the setting below.
    /// forge-config: default.fuzz.runs = 64
    /// forge-config: deep.fuzz.runs = 5000
    function testFuzz_a_bank_run_pays_everyone_their_fraction(
        uint256 seed_,
        uint256 count,
        uint8 bookKind
    ) public {
        seed = seed_;
        count = bound(count, 3, 12);
        // Spot bought outright, a levered perp, or spot beside a levered perp.
        uint8 book = bookKind % 3;
        bool levered = book != 0;

        // The holders join.
        for (uint256 i = 0; i < count; i++) {
            (address who, uint256 key) = makeAddrAndKey(string.concat("holder ", vm.toString(i)));
            keyFor[who] = key;
            uint256 amount = 1000e6 + rnd() % 200_000e6;
            give(who, amount);
            vm.startPrank(who);
            usd.approve(address(vault), amount);
            vault.requestDeposit(amount, who, sign(key, vault.statementDigest(who)));
            vm.stopPrank();
            leavers.push(who);
        }
        value(vault);

        // The book: most of the cash goes to the venue, spot bought outright,
        // a perp at two to four times the equity, or both.
        uint256 cash = vault.cash();
        uint256 deployed = cash * (70 + rnd() % 24) / 100;
        vm.startPrank(executor);
        station.pull(deployed);
        station.deposit("local", deployed);
        OrderRequest[] memory orders = new OrderRequest[](book == 2 ? 2 : 1);
        if (book == 2) {
            orders[0] = OrderRequest({
                market: SPOT,
                isBuy: true,
                size: deployed * 40 / 100 * 1e18 / PRICE,
                limitPrice: PRICE,
                reduceOnly: false
            });
            orders[1] = OrderRequest({
                market: PERP,
                isBuy: true,
                size: deployed * (2 + rnd() % 3) * 1e18 / PRICE,
                limitPrice: PRICE,
                reduceOnly: false
            });
        } else if (levered) {
            uint256 notional = deployed * (2 + rnd() % 3);
            orders[0] = OrderRequest({
                market: PERP,
                isBuy: true,
                size: notional * 1e18 / PRICE,
                limitPrice: PRICE,
                reduceOnly: false
            });
        } else {
            orders[0] = OrderRequest({
                market: SPOT,
                isBuy: true,
                size: deployed * 95 / 100 * 1e18 / PRICE,
                limitPrice: PRICE,
                reduceOnly: false
            });
        }
        venue.place(address(station), orders);
        vm.stopPrank();

        // A gain, then payday: the author and the platform hold fee shares.
        base = PRICE * (10_300 + rnd() % 800) / 10_000;
        market(base, 0);
        value(vault);
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        value(vault);
        assertGt(vault.balanceOf(author), 0, "payday minted the author nothing");
        assertGt(vault.balanceOf(platform), 0, "payday minted the platform nothing");
        leavers.push(author);
        leavers.push(platform);
        shuffle();
        tankAtStart = vault.tank().balance;

        // The run.
        uint256 next;
        for (uint256 round = 0; round < 120; round++) {
            bool calm = round >= 40;
            wide = !calm && rnd() % 3 == 0;
            if (wide) wideRounds++;
            uint256 spreadBps = wide ? 300 + rnd() % 300 : rnd() % 160;
            market(base * (9700 + rnd() % 601) / 10_000, spreadBps);

            uint256 asking = 1 + rnd() % 3;
            for (uint256 k = 0; k < asking && next < leavers.length; k++) {
                address who = leavers[next++];
                uint256 shares = vault.balanceOf(who);
                if (shares == 0) continue;
                asked[who] = true;
                vm.prank(who);
                vault.requestRedeem(shares, who);
            }

            settleRound();
            tryClaims();
            if (wide && rnd() % 2 == 0) fallBack();
            serveUnits();
            // The strategy raises cash for the leavers now and then: on a
            // levered book a claim's own close can't bring its worth home
            // while the venue keeps it as margin, so this is how they are
            // paid.
            if (rnd() % 3 == 0) raiseCash();
            assertEq(vault.tank().balance, tankAtStart, "a withdrawal touched the tank");
            if (next == leavers.length && quiet()) break;
        }
        assertTrue(next == leavers.length && quiet(), "the run never came to rest");
        windDownAndClose();
        emit log_named_uint("leavers", leavers.length);
        emit log_named_uint("withdrawals settled at a valuation", settledExits);
        emit log_named_uint("claims paid at their close", closedClaims);
        emit log_named_uint("fallbacks", fallbacks);
        emit log_named_uint("rounds outside the band", wideRounds);
    }

    // The steps ---------------------------------------------------------------

    /// Value at the next slot and settle it. What the valuation records is
    /// the pool as the run read it just before, from the station and the
    /// cash: at the mark, and at the lower of the mark and the exit side.
    /// Each payout is at most the leaver's shares at that exit side, and
    /// the holders who stay keep their exit-side price.
    function settleRound() internal {
        nextSlot();
        (uint256 markNow, uint256 exitNow) = station.value();
        (uint256 unitsNow,) = vault.units();
        uint256 cashNow = vault.cash();
        vault.recordValuation();
        Valuation memory v = vault.valuationAt(vault.valuationCount() - 1);
        if (v.status != ValuationStatus.Priced) return;
        if (exitNow > markNow) exitNow = markNow;
        assertEq(
            v.gross,
            cashNow + markNow * (WAD - unitsNow) / WAD + 1,
            "a valuation recorded other than the pool"
        );
        assertEq(
            v.grossExit,
            cashNow + exitNow * (WAD - unitsNow) / WAD + 1,
            "a valuation recorded other than the pool at exit prices"
        );
        uint256 count = vault.requestCount();
        uint256[] memory before = new uint256[](count + 1);
        bool[] memory open = new bool[](count + 1);
        for (uint256 id = 1; id <= count; id++) {
            Request memory r = vault.requestAt(id);
            (open[id], before[id]) = (!r.done, usd.balanceOf(r.owner));
        }
        uint256 stayers = exitPerShare();
        vault.settle(type(uint256).max);
        for (uint256 id = 1; id <= count; id++) {
            Request memory r = vault.requestAt(id);
            if (!open[id] || !r.done) continue;
            uint256 shares = r.kind == RequestKind.Claim ? vault.claimAt(r.ref).shares : r.amount;
            uint256 paid = usd.balanceOf(r.owner) - before[id];
            assertLe(paid, shares * v.grossExit / v.supply + 1, "a leaver was paid above their fraction");
            settledExits++;
            // Every withdrawal of one slot, the fee holders' too, is paid the
            // same per share, up to the rounding of each payout: fee shares
            // get no priority, and nobody is paid ahead of anyone else.
            if (r.kind != RequestKind.Redeem || paid == 0) continue;
            for (uint256 k = 0; k < slotShares.length; k++) {
                uint256 x = paid * slotShares[k];
                uint256 y = slotPaid[k] * shares;
                assertLe(
                    x > y ? x - y : y - x,
                    3 * (shares + slotShares[k]),
                    "two leavers of one slot were paid different prices"
                );
            }
            slotShares.push(shares);
            slotPaid.push(paid);
        }
        delete slotShares;
        delete slotPaid;
        assertGe(exitPerShare() * (1e9 + 1) / 1e9 + 1, stayers, "a stayer paid for someone's exit");
    }

    /// A stranger tries every open claim. A paid claim got at most its
    /// fraction at the exit side, its close sold at most its fraction of
    /// each position, and the stayers kept their price.
    function tryClaims() internal {
        uint256 count = vault.claimCount();
        for (uint256 id = 1; id <= count; id++) {
            Claim memory c = vault.claimAt(id);
            if (c.state != ClaimState.Pending && c.state != ClaimState.Waiting) continue;
            (uint256 units,) = vault.units();
            uint256 supply = vault.totalSupply() + OFFSET;
            (, uint256 exit) = station.value();
            uint256 fair = (vault.cash() + exit * (WAD - units) / WAD) * c.shares / supply;
            uint256 fraction = c.shares * (WAD - units) / supply;
            int256 spot = venue.positionOf(address(station), SPOT).size;
            int256 perp = venue.positionOf(address(station), PERP).size;
            uint256 stayers = exitPerShare();
            uint256 had = usd.balanceOf(c.owner);
            vm.prank(stranger);
            try vault.settleClaim(id) {} catch {}
            Claim memory after_ = vault.claimAt(id);
            if (after_.state != ClaimState.Paid) continue;
            closedClaims++;
            assertLe(usd.balanceOf(c.owner) - had, fair + 1, "a claim was paid above its fraction");
            assertGe(
                venue.positionOf(address(station), SPOT).size,
                spot - int256(ceilFraction(uint256(spot), fraction)),
                "a claim closed more spot than its fraction"
            );
            assertGe(
                venue.positionOf(address(station), PERP).size,
                perp - int256(ceilFraction(uint256(perp), fraction)),
                "a claim closed more of the perp than its fraction"
            );
            assertGe(exitPerShare() * (1e9 + 1) / 1e9 + 1, stayers, "a stayer paid for a claim's close");
        }
    }

    /// The market stays outside the band for a day: whoever is waiting
    /// takes the fallback or books a claim instead. Each is paid at most
    /// its share of the cash now.
    function fallBack() internal {
        vm.warp(vm.getBlockTimestamp() + 1 days + SLOT);
        // The executor renews its blessing and its venue approval.
        blessExecutorKey(executor, address(vault), vault.bundleHash());
        vm.prank(executor);
        station.approveKey("local", executor);
        uint256 count = vault.requestCount();
        for (uint256 id = 1; id <= count; id++) {
            Request memory r = vault.requestAt(id);
            if (r.done || r.kind != RequestKind.Redeem) continue;
            vm.prank(r.owner);
            if (rnd() % 2 == 0) {
                uint256 most = vault.cash() * r.amount / (vault.totalSupply() + OFFSET);
                uint256 had = usd.balanceOf(r.owner);
                try vault.takeProRata(id) {
                    fallbacks++;
                    assertLe(
                        usd.balanceOf(r.owner) - had, most + 1, "the fallback paid above the cash's share"
                    );
                } catch {}
            } else {
                try vault.claimInstead(id) {} catch {}
            }
        }
        count = vault.claimCount();
        for (uint256 id = 1; id <= count; id++) {
            Claim memory c = vault.claimAt(id);
            if (c.state != ClaimState.Pending && c.state != ClaimState.Waiting) continue;
            if (rnd() % 2 != 0) continue;
            uint256 most = vault.cash() * c.shares / (vault.totalSupply() + OFFSET);
            uint256 had = usd.balanceOf(c.owner);
            vm.prank(c.owner);
            try vault.takeProRataClaim(id) {
                fallbacks++;
                assertLe(
                    usd.balanceOf(c.owner) - had, most + 1, "a claim's fallback paid above the cash's share"
                );
            } catch {}
        }
    }

    /// Serve every unit holder (a stranger asks) and let them collect.
    function serveUnits() internal {
        for (uint256 i = 0; i < leavers.length; i++) {
            address who = leavers[i];
            (uint256 raw,) = vault.unitsOf(who);
            if (raw != 0) {
                vm.prank(stranger);
                try vault.serveProRata(who) {} catch {}
            }
            (, uint256 claimable) = vault.unitsOf(who);
            if (claimable != 0) {
                vm.prank(who);
                vault.collectUnits();
            }
        }
    }

    /// Sell part of every position, take what the venue lets go of and
    /// send it home.
    function raiseCash() internal {
        uint256 part = 20 + rnd() % 61;
        bytes32[2] memory markets = [SPOT, PERP];
        vm.startPrank(executor);
        for (uint256 i = 0; i < 2; i++) {
            int256 size = venue.positionOf(address(station), markets[i]).size;
            if (size <= 0) continue;
            OrderRequest[] memory orders = new OrderRequest[](1);
            orders[0] = OrderRequest({
                market: markets[i],
                isBuy: false,
                size: uint256(size) * part / 100,
                limitPrice: 0,
                reduceOnly: true
            });
            venue.place(address(station), orders);
        }
        int256 cash = venue.cashOf(address(station));
        if (cash > 0) station.withdraw("local", uint256(cash));
        uint256 idle = usd.balanceOf(address(station));
        if (idle != 0) station.push(idle);
        vm.stopPrank();
    }

    /// Nothing is owed: every request done, every claim paid or taken.
    function quiet() internal view returns (bool) {
        uint256 count = vault.requestCount();
        for (uint256 id = 1; id <= count; id++) {
            if (!vault.requestAt(id).done) return false;
        }
        count = vault.claimCount();
        for (uint256 id = 1; id <= count; id++) {
            ClaimState s = vault.claimAt(id).state;
            if (s != ClaimState.Paid && s != ClaimState.Fallback) return false;
        }
        return true;
    }

    /// Everyone has left; what remains belongs to the unit holders, the
    /// dead shares and the fee shares the leavers' slices paid. Bring it
    /// all home, serve every unit, close, and pay out the rest.
    function windDownAndClose() internal {
        for (uint256 i = 0; i < leavers.length; i++) {
            address who = leavers[i];
            if (who == author || who == platform) continue;
            assertEq(vault.balanceOf(who), 0, "a holder who left still holds shares");
        }
        assertEq(vault.escrow(), 0);

        vm.prank(admin);
        vault.kill();
        market(base, 0);
        skip(HALT_GRACE + 1);
        station.stop();
        (uint256 m,) = station.value();
        if (m != 0) station.unwind(WAD);
        // The units' part of it came home to their reserve. Once a valuation
        // has seen the station empty, every unit is served (an empty station
        // clears them) and collected.
        value(vault);
        serveUnits();
        for (uint256 i = 0; i < leavers.length; i++) {
            (uint256 raw, uint256 claimable) = vault.unitsOf(leavers[i]);
            assertEq(raw + claimable, 0, "a unit holder is still owed");
        }
        (uint256 units, uint256 reserve) = vault.units();
        assertEq(units, 0, "units are still outstanding");
        assertLe(reserve, 1e3, "a unit reserve is left");
        uint256 deadPart = vault.cash() * vault.balanceOf(DEAD) / vault.totalSupply();
        vault.close();
        address[2] memory feeHolders = [author, platform];
        for (uint256 i = 0; i < 2; i++) {
            uint256 shares = vault.balanceOf(feeHolders[i]);
            if (shares == 0) continue;
            vm.prank(feeHolders[i]);
            vault.redeemClosed(shares, feeHolders[i]);
        }
        assertEq(vault.totalSupply(), vault.balanceOf(DEAD), "shares remain that nobody can redeem");
        // The dead shares' part went to the author's tank at the close.
        assertLe(
            vault.tank().balance, tankAtStart + deadPart, "the close paid the author more than was theirs"
        );
        assertGe(vault.tank().balance, tankAtStart, "the close took from the tank");
        uint256 books = vault.cash() + vault.tank().balance;
        assertGe(usd.balanceOf(address(vault)), books, "the vault ends short");
        assertLe(usd.balanceOf(address(vault)), books + 1e3, "the vault ends holding money nobody is owed");
    }

    // Helpers -----------------------------------------------------------------

    /// Worth per share at the exit side, the units' part of the station
    /// set aside, scaled by 1e18.
    function exitPerShare() internal view returns (uint256) {
        (, uint256 exit) = station.value();
        (uint256 units,) = vault.units();
        return (vault.cash() + exit * (WAD - units) / WAD + 1) * 1e18 / (vault.totalSupply() + OFFSET);
    }

    function ceilFraction(uint256 size, uint256 fraction) internal pure returns (uint256) {
        return (size * fraction + WAD - 1) / WAD;
    }

    /// Both markets at `p`, the bid and ask `spreadBps` apart around it.
    function market(uint256 p, uint256 spreadBps) internal {
        uint256 half = p * spreadBps / 20_000;
        vm.startPrank(operator);
        venue.setPrices(SPOT, p, p, p - half, p + half);
        venue.setPrices(PERP, p, p, p - half, p + half);
        vm.stopPrank();
    }

    function nextSlot() internal {
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
    }

    function shuffle() internal {
        for (uint256 i = leavers.length - 1; i > 0; i--) {
            uint256 j = rnd() % (i + 1);
            (leavers[i], leavers[j]) = (leavers[j], leavers[i]);
        }
    }

    function sign(uint256 key, bytes32 digest) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return abi.encodePacked(r, s, v);
    }

    function nextMondayAfter(uint256 t) internal pure returns (uint256) {
        uint256 day = t / 1 days;
        return (day - ((day + 3) % 7)) * 1 days + 7 days;
    }
}
