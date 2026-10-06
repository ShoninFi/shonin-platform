// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalUsd} from "../../src/LocalUsd.sol";
import {DelayedVenue} from "../../src/drivers/DelayedVenue.sol";
import {ValuationStatus} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";
import {MockHyperCore} from "./MockHyperCore.sol";

/// The deposit contract as the venue runs it: the dollars leave the station
/// in the transaction, and HyperCore credits the account after the block.
/// Until then the read precompiles, which answer as of the start of the
/// block, show nothing of them.
contract LateDepositWallet {
    MockHyperCore private immutable core;
    LocalUsd private immutable usd;
    mapping(address => uint256) public onItsWay;

    constructor(MockHyperCore core_, LocalUsd usd_) {
        core = core_;
        usd = usd_;
    }

    function deposit(uint256 amount, uint32) external {
        usd.transferFrom(msg.sender, address(this), amount);
        onItsWay[msg.sender] += amount;
    }

    function land(address who) external {
        core.credit(who, int256(onItsWay[who]));
        onItsWay[who] = 0;
    }
}

/// Dollars the executor has sent from the station to its HyperCore account
/// are, for the rest of that block at least, in neither: gone from the
/// station, not yet in what the precompiles report. A valuation recorded
/// while they may be on their way prices nothing, so nobody's deposit buys
/// the pool for a fraction of its worth and nobody leaving is paid a
/// fraction; the next one after the credit window prices the true worth.
/// And the executor's moves are spaced, so they can't void every valuation.
contract DollarsOnTheirWayPriceNothingTest is HyperliquidVault {
    function test_a_valuation_in_the_block_of_a_deposit_to_the_venue_is_void_and_the_next_prices_the_true_worth(
    ) public {
        vm.etch(address(wallet), address(new LateDepositWallet(core, usd)).code);
        LateDepositWallet late = LateDepositWallet(address(wallet));

        depositAs(vault, alice, 100_000e6);
        // Bob asks to put in 50k, to be priced at the next slot.
        requestDepositAs(vault, bob, 50_000e6);

        // At that slot's boundary the executor moves 90k to the venue, and in
        // the same block the slot's valuation is recorded (by the keeper, as
        // every slot, or by anyone who was waiting for the move).
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vm.startPrank(executor);
        station.pull(90_000e6);
        station.deposit(HL, 90_000e6);
        vm.stopPrank();
        assertEq(late.onItsWay(address(station)), 90_000e6, "on their way to HyperCore");
        vault.recordValuation();
        vault.settle(type(uint256).max);
        uint256 index = vault.valuationCount() - 1;
        assertTrue(vault.valuationAt(index).status == ValuationStatus.Void, "priced nothing");
        assertEq(vault.balanceOf(bob), 0, "Bob's deposit waits");

        // After the block the venue credits the account; once the credit
        // window has passed, the next valuation prices the true worth.
        vm.roll(block.number + 1);
        skip(1);
        late.land(address(station));
        skip(hl.CREDIT_WINDOW());
        value(vault);
        index = vault.valuationCount() - 1;
        assertTrue(vault.valuationAt(index).status == ValuationStatus.Priced);

        // Bob paid 50k a block ago and nothing has moved in any market.
        assertLe(_worthOf(bob), 50_000e6, "a newcomer's shares are worth no more than he paid for them");
        assertGe(_worthOf(alice), 99_000e6, "and the holder already there has lost nothing");
    }

    function test_the_executors_moves_through_the_venue_are_spaced_so_a_valuation_always_fits_between()
        public
    {
        depositAs(vault, alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(90_000e6);
        station.deposit(HL, 40_000e6);
        // A second move inside the credit window and one slot: refused.
        skip(hl.CREDIT_WINDOW() + SLOT);
        vm.expectRevert(DelayedVenue.TooSoon.selector);
        station.deposit(HL, 10_000e6);
        vm.expectRevert(DelayedVenue.TooSoon.selector);
        station.withdraw(HL, 10_000e6);
        vm.stopPrank();
        // Meanwhile a valuation is priced once the window is over.
        value(vault);
        assertTrue(vault.valuationAt(vault.valuationCount() - 1).status == ValuationStatus.Priced);
        // After the credit window, a slot and its window: allowed.
        skip(3);
        vm.prank(executor);
        station.deposit(HL, 10_000e6);
        assertFalse(hl.priced(address(station)), "and it voids valuations for its own window");
        assertTrue(station.outsideBand());
    }
}
