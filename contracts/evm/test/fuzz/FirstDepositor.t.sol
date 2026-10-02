// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {Vault} from "../../src/Vault.sol";
import {VaultFactory} from "../../src/VaultFactory.sol";
import {Valuation, ValuationStatus} from "../../src/interfaces/IVault.sol";
import {ChainName, VaultConfig} from "../../src/interfaces/IVaultFactory.sol";
import {Fixture} from "../Fixture.sol";

/// An open single-chain vault and the helpers the donation tests share.
abstract contract DonationBase is Fixture {
    uint256 internal constant OFFSET = 1e12;
    address internal constant DEAD = 0x000000000000000000000000000000000000dEaD;

    Vault internal vault;
    Station internal station;

    function setUp() public virtual override {
        super.setUp();
        (vault,) = openVault();
        station = stationOf(vault);
    }

    function latest() internal view returns (Valuation memory) {
        return vault.valuationAt(vault.valuationCount() - 1);
    }

    function ceilDiv(uint256 a, uint256 b) internal pure returns (uint256) {
        return a == 0 ? 0 : (a - 1) / b + 1;
    }

    function nextMondayAfter(uint256 t) internal pure returns (uint256) {
        uint256 day = t / 1 days;
        return (day - ((day + 3) % 7)) * 1 days + 7 days;
    }
}

/// What a vault is worth comes from its last valuation, never from its own
/// balance, and every vault starts with dead shares nobody can redeem on top
/// of a virtual offset. So money sent to it changes nothing, money sent to
/// its home station is everyone's in proportion, and nobody can round the
/// next depositor's shares away.
contract FirstDepositorTest is DonationBase {
    /// Money sent where nothing counts it: the vault's own address, the
    /// venue's contract, or both.
    function donateUncounted(uint256 amount, uint8 where) internal {
        if (where % 3 != 1) give(address(vault), amount);
        if (where % 3 != 0) give(address(venue), amount);
    }

    struct Outcome {
        uint256 aliceShares;
        uint256 bobShares;
        uint256 price;
        uint256 total;
        uint256 exit;
        uint256 mark;
        uint256 cash;
        uint256 bobPaid;
    }

    /// Alice deposits, part of the pool goes to the venue, bob deposits and
    /// then leaves, with `donation` sent where nothing counts it before
    /// alice's deposit, or between it and its valuation.
    function play(
        uint256 first,
        uint256 second,
        uint256 donation,
        uint8 where,
        bool early
    ) internal returns (Outcome memory o) {
        if (early) donateUncounted(donation, where);
        requestDepositAs(vault, alice, first);
        if (!early) donateUncounted(donation, where);
        value(vault);
        vm.startPrank(executor);
        station.pull(first / 2);
        station.approveKey("local", executor);
        station.deposit("local", first / 4);
        vm.stopPrank();
        o.bobShares = depositAs(vault, bob, second);
        o.aliceShares = vault.balanceOf(alice);
        o.price = vault.sharePrice();
        o.total = vault.totalValue();
        o.exit = vault.exitValue();
        o.mark = vault.mark();
        o.bobPaid = redeemAs(vault, bob, o.bobShares);
        o.cash = vault.cash();
    }

    /// The same deposits and withdrawal with and without a donation of any
    /// size to the vault or the venue: every share, price and payout is the
    /// same to the unit.
    function testFuzz_a_donation_to_the_vault_or_the_venue_changes_nothing(
        uint256 first,
        uint256 second,
        uint256 donation,
        uint8 where,
        bool early
    ) public {
        first = bound(first, MINIMUM_DEPOSIT, 10_000_000e6);
        second = bound(second, MINIMUM_DEPOSIT, 10_000_000e6);
        donation = bound(donation, 1, 1e30);

        uint256 snapshot = vm.snapshotState();
        Outcome memory without = play(first, second, 0, where, early);
        vm.revertToState(snapshot);
        Outcome memory with = play(first, second, donation, where, early);

        assertEq(with.aliceShares, without.aliceShares, "a donation changed the first depositor's shares");
        assertEq(with.bobShares, without.bobShares, "a donation changed the next depositor's shares");
        assertEq(with.price, without.price, "a donation moved the share price");
        assertEq(with.total, without.total, "a donation counted in the vault's value");
        assertEq(with.exit, without.exit, "a donation counted in the vault's exit value");
        assertEq(with.mark, without.mark, "a donation moved the mark");
        assertEq(with.bobPaid, without.bobPaid, "a donation changed what a leaver was paid");
        assertEq(with.cash, without.cash, "a donation was counted as cash");
    }

    /// Money sent to the home station is counted at the next valuation, as
    /// everyone's in proportion: no holder's shares change, and the next
    /// depositor buys at the price it set, losing at most one share's worth
    /// to rounding.
    function testFuzz_a_donation_to_the_home_station_is_shared_by_every_holder(
        uint256 first,
        uint256 donation,
        uint256 second
    ) public {
        first = bound(first, MINIMUM_DEPOSIT, 10_000_000e6);
        donation = bound(donation, 1, 1e18);
        second = bound(second, MINIMUM_DEPOSIT, 10_000_000e6);
        uint256 aliceShares = depositAs(vault, alice, first);
        uint256 dead = vault.balanceOf(DEAD);
        uint256 priceBefore = vault.sharePrice();

        give(address(station), donation);
        value(vault);
        assertEq(vault.balanceOf(alice), aliceShares, "a donation changed a holder's shares");
        assertEq(vault.balanceOf(DEAD), dead, "a donation changed the dead shares");
        // The holding fee for one slot can't outweigh a donation of a unit.
        assertGe(vault.sharePrice() + 1, priceBefore, "a donation lowered the share price");

        requestDepositAs(vault, bob, second);
        value(vault);
        Valuation memory v = latest();
        uint256 bobShares = vault.balanceOf(bob);
        uint256 net = second - ceilDiv(second * 10, 10_000);
        uint256 perShare = ceilDiv(v.gross - v.fee, v.supply);
        assertGt(bobShares, 0, "the next depositor was rounded to nothing");
        // What bob's shares are worth on the pool he bought into.
        uint256 worth = bobShares * (v.gross - v.fee) / v.supply;
        assertLe(worth, net, "the next depositor bought more than they paid for");
        assertGe(worth + perShare + 1, net, "the next depositor was rounded down by more than a share");
    }

    /// Dead shares are minted once and never move, and every valuation
    /// counts every share plus the virtual offset and every dollar plus one,
    /// through deposits, withdrawals, donations and paydays.
    function testFuzz_dead_shares_and_the_virtual_offset_hold_at_every_valuation(uint256 seed) public {
        uint256 dead = DEAD_ASSETS * OFFSET;
        assertEq(vault.balanceOf(DEAD), dead, "the dead shares are not a cent's worth");
        assertEq(vault.sharePrice(), 1e6, "a share doesn't start at a dollar");
        for (uint256 step = 0; step < 16; step++) {
            uint256 r = uint256(keccak256(abi.encode(seed, step)));
            uint256 action = r % 6;
            r >>= 8;
            if (action == 0) {
                requestDepositAs(vault, r % 2 == 0 ? alice : bob, MINIMUM_DEPOSIT + (r >> 8) % 1_000_000e6);
            } else if (action == 1) {
                address who = r % 2 == 0 ? alice : bob;
                uint256 held = vault.balanceOf(who);
                if (held > 1) {
                    vm.prank(who);
                    vault.requestRedeem(held / (1 + r % 4), who);
                }
            } else if (action == 2) {
                give(r % 2 == 0 ? address(vault) : address(station), (r >> 8) % 1e15);
            } else if (action == 3) {
                vm.warp(nextMondayAfter(vault.lastPayment()) + (r >> 8) % 1 days);
            } else {
                uint256 t = vm.getBlockTimestamp();
                vm.warp(t - (t % SLOT) + SLOT);
                vault.recordValuation();
                Valuation memory v = latest();
                assertTrue(v.status == ValuationStatus.Priced);
                (uint256 atMark,) = station.value();
                assertEq(v.supply, vault.totalSupply() + OFFSET, "a valuation lost the virtual offset");
                assertEq(v.cash, vault.cash());
                assertEq(v.gross, v.cash + atMark + 1, "a valuation lost the virtual dollar");
                vault.settle(type(uint256).max);
            }
            assertEq(vault.balanceOf(DEAD), dead, "the dead shares moved");
            assertGe(vault.totalSupply(), dead);
        }
    }
}

/// A first deposit of a single unit of the dollar, followed by money sent to
/// the vault or its station, never rounds the next depositor's shares away.
/// On a vault whose minimum deposit is one unit and which keeps no
/// transaction fee, so nothing but the dead shares and the offset stand in
/// the way.
contract FirstDepositorOneUnitTest is DonationBase {
    function newFactory(
        VaultFactory.Deps memory deps,
        VaultConfig memory config,
        ChainName[] memory chains
    ) internal override returns (VaultFactory) {
        config.minimumDeposit = 1;
        config.transactionBps = 0;
        return new VaultFactory(deps, config, chains);
    }

    /// A one-unit first deposit, then a donation of any size to the vault,
    /// to the home station or split between them: the next depositor either
    /// buys shares worth what they paid, less at most one share's worth, or
    /// is refunded in full; a share is never worth more than the donation
    /// spread over the dead shares; and the first depositor's shares are
    /// worth no more than they and the donation put in.
    function testFuzz_a_one_unit_first_deposit_and_a_donation_never_round_the_next_depositor_down(
        uint256 units,
        uint256 donation,
        uint256 deposit,
        uint8 where
    ) public {
        units = bound(units, 1, 1000);
        donation = bound(donation, 0, 1e18);
        deposit = bound(deposit, 1, 1e15);
        uint256 bobShares = depositAs(vault, bob, units);
        assertGt(bobShares, 0, "a one-unit deposit bought nothing");

        uint256 toStation = where % 3 == 0 ? 0 : where % 3 == 1 ? donation : donation / 2;
        give(address(vault), donation - toStation);
        give(address(station), toStation);
        value(vault);
        Valuation memory v = latest();
        uint256 perShare = ceilDiv(v.gross - v.fee, v.supply);
        // Dead shares are a cent's worth at a dollar a share: the donation
        // can't make one share worth more than itself spread over them.
        assertLe(
            perShare,
            toStation / (DEAD_ASSETS * OFFSET) + 2,
            "a share was priced above the dead shares' bound"
        );

        uint256 aliceUsd = usd.balanceOf(alice) + deposit;
        uint256 aliceShares = depositAs(vault, alice, deposit);
        v = latest();
        if (aliceShares == 0) {
            assertEq(usd.balanceOf(alice), aliceUsd, "a deposit too small for a share was kept");
        } else {
            uint256 worth = aliceShares * (v.gross - v.fee) / v.supply;
            assertLe(worth, deposit, "the next depositor bought more than they paid for");
            assertGe(
                worth + perShare + 1, deposit, "the next depositor was rounded down by more than a share"
            );
        }
        uint256 bobWorth = bobShares * (v.gross - v.fee) / v.supply;
        assertLe(bobWorth, units + donation, "the first depositor's shares are worth more than went in");
    }
}
