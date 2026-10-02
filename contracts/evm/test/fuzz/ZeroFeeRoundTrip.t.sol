// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {Vault} from "../../src/Vault.sol";
import {VaultFactory} from "../../src/VaultFactory.sol";
import {OrderRequest} from "../../src/interfaces/ILocalVenue.sol";
import {ClaimState} from "../../src/interfaces/IVault.sol";
import {ChainName, VaultConfig} from "../../src/interfaces/IVaultFactory.sol";
import {Fixture} from "../Fixture.sol";

/// With no transaction fee, no holding fee and no spread, a deposit followed
/// by a full withdrawal never returns more than was put in, directly or
/// through a claim, at any size from one unit of the dollar: only the
/// direction each division rounds stands between a round trip and a profit,
/// and it always favours the pool. The profit fee stays on, so its slice and
/// the mark blend are exercised.
contract ZeroFeeRoundTripTest is Fixture {
    Vault internal vault;
    Station internal station;

    function newFactory(
        VaultFactory.Deps memory deps,
        VaultConfig memory config,
        ChainName[] memory chains
    ) internal override returns (VaultFactory) {
        config.minimumDeposit = 1;
        config.transactionBps = 0;
        config.holdingBps = 0;
        return new VaultFactory(deps, config, chains);
    }

    function setUp() public override {
        super.setUp();
        (vault,) = openVault();
        station = stationOf(vault);
        give(address(venue), 100_000_000e6);
        // No idle floor, so the claim path can be reached at any size.
        vm.prank(author);
        vault.setIdleFloor(0);
        skip(1 days);
        vm.prank(executor);
        station.approveKey("local", executor);
    }

    function nextMondayAfter(uint256 t) internal pure returns (uint256) {
        uint256 day = t / 1 days;
        return (day - ((day + 3) % 7)) * 1 days + 7 days;
    }

    /// A pool with a first holder, part of it in BTC at the venue, the market
    /// moved up (a profit fee owed) or down (under the mark), optionally
    /// across a payday, and optionally a second holder who joined above the
    /// mark (so the mark is a blend).
    function arrange(uint256 first, uint256 move, bool up, bool payday, uint256 joiner) internal {
        depositAs(vault, alice, first);
        uint256 out = vault.cash() * 3 / 4;
        if (out >= 1e6) {
            vm.startPrank(executor);
            station.pull(out);
            station.deposit("local", out);
            OrderRequest[] memory orders = new OrderRequest[](1);
            orders[0] = OrderRequest({
                market: SPOT,
                isBuy: true,
                size: out / 2 * 1e18 / PRICE,
                limitPrice: PRICE,
                reduceOnly: false
            });
            venue.place(address(station), orders);
            vm.stopPrank();
        }
        uint256 p = up ? PRICE * (10_000 + move) / 10_000 : PRICE * (10_000 - move) / 10_000;
        setPrice(SPOT, p, 0);
        value(vault);
        if (payday) {
            vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
            value(vault);
            setPrice(SPOT, up ? p * 10_100 / 10_000 : p * 9900 / 10_000, 0);
            value(vault);
        }
        if (joiner != 0) depositAs(vault, author, joiner);
    }

    /// Settled at a valuation: the withdrawal is paid from cash.
    function testFuzz_a_round_trip_with_no_fees_never_profits(
        uint256 first,
        uint256 move,
        bool up,
        bool payday,
        uint256 joiner,
        uint256 amount
    ) public {
        first = bound(first, 1, 10_000_000e6);
        move = bound(move, 0, 3000);
        joiner = joiner % 3 == 0 ? 0 : bound(joiner, 1, 1_000_000e6);
        amount = bound(amount, 1, 10_000_000e6);
        arrange(first, move, up, payday, joiner);

        uint256 shares = depositAs(vault, bob, amount);
        if (shares == 0) {
            assertEq(usd.balanceOf(bob), amount, "a deposit that bought nothing kept money");
            return;
        }
        uint256 paid = redeemAs(vault, bob, shares);
        assertEq(vault.balanceOf(bob), 0);
        assertLe(paid, amount, "a fee-free round trip returned more than went in");
    }

    /// The withdrawal can't be paid from cash and is served by a close.
    function testFuzz_a_round_trip_through_a_claim_with_no_fees_never_profits(
        uint256 first,
        uint256 move,
        bool up,
        uint256 amount
    ) public {
        first = bound(first, 1000e6, 10_000_000e6);
        move = bound(move, 0, 3000);
        amount = bound(amount, 1, 10_000_000e6);
        arrange(first, move, up, false, 0);

        uint256 shares = depositAs(vault, bob, amount);
        if (shares == 0) return;
        // Everything but a dollar goes to the station, so bob's exit is a
        // claim.
        uint256 cash = vault.cash();
        if (cash > 1e6) {
            vm.prank(executor);
            station.pull(cash - 1e6);
        }
        uint256 before = usd.balanceOf(bob);
        vm.prank(bob);
        vault.requestRedeem(shares, bob);
        value(vault);
        uint256 paid = usd.balanceOf(bob) - before;
        uint256 id = vault.claimCount();
        if (id != 0 && vault.claimAt(id).owner == bob && vault.claimAt(id).state != ClaimState.Paid) {
            vault.settleClaim(id);
            if (vault.claimAt(id).state == ClaimState.Queued) value(vault);
            assertTrue(vault.claimAt(id).state == ClaimState.Paid, "the claim was not paid");
            paid = usd.balanceOf(bob) - before;
        }
        assertLe(paid, amount, "a fee-free round trip through a claim returned more than went in");
    }
}
