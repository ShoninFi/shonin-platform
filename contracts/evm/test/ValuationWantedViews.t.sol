// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IVault, ValuationStatus, VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// What the vault says about whether a valuation is wanted: the newest priced
/// valuation, the oldest request still waiting for its price, and whether a
/// halted vault's close waits for one. Each is read in one call, and each
/// matches what the vault itself then does.
contract ValuationWantedViewsTest is ClaimBase {
    function atSlot() internal returns (uint64 time) {
        uint256 t = vm.getBlockTimestamp();
        time = uint64(t - (t % SLOT) + SLOT);
        vm.warp(time);
    }

    function test_last_priced_names_the_newest_priced_valuation_not_a_void_one() public {
        (bool priced, uint256 index, uint64 time) = vault.lastPriced();
        assertFalse(priced, "priced before any valuation");
        assertEq(index, 0);
        assertEq(time, 0);

        depositAs(vault, alice, 100_000e6);
        deploySpot(90_000e6, 90_000e6, 1e18);
        value(vault);
        uint256 pricedIndex = vault.valuationCount() - 1;
        uint64 pricedTime = latest().time;
        (priced, index, time) = vault.lastPriced();
        assertTrue(priced);
        assertEq(index, pricedIndex);
        assertEq(time, pricedTime);

        // A market outside the band voids the next valuations: the newest
        // priced one is still the one before them.
        setPrice(SPOT, PRICE, 2000);
        value(vault);
        value(vault);
        assertTrue(latest().status == ValuationStatus.Void);
        (priced, index, time) = vault.lastPriced();
        assertTrue(priced);
        assertEq(index, pricedIndex);
        assertEq(time, pricedTime);
        assertEq(vault.valuationAt(index).time, time);

        // Back inside the band, the next one is.
        setPrice(SPOT, PRICE, 0);
        value(vault);
        (priced, index, time) = vault.lastPriced();
        assertEq(index, vault.valuationCount() - 1);
        assertEq(time, latest().time);
    }

    function test_next_unpriced_is_the_oldest_request_no_valuation_has_priced() public {
        depositAs(vault, alice, 100_000e6);
        assertEq(vault.nextUnpriced(), 0, "a request waits after everything was priced");

        // Asked for at a slot's start, Bob's deposit is priced at the next
        // slot; Alice's, six seconds on, at the one after.
        uint64 slot = atSlot();
        uint256 his = requestDepositAs(vault, bob, 1000e6);
        skip(6);
        uint256 hers = requestDepositAs(vault, alice, 1000e6);
        assertEq(vault.requestAt(his).earliest, slot + SLOT);
        assertEq(vault.requestAt(hers).earliest, slot + 2 * SLOT);
        assertEq(vault.nextUnpriced(), slot + SLOT);

        // A valuation at the next slot prices Bob's and not hers.
        vm.warp(slot + SLOT);
        vault.recordValuation();
        assertEq(vault.nextUnpriced(), slot + 2 * SLOT, "priced and not yet settled is not waiting");
        vault.settle(type(uint256).max);
        assertTrue(vault.requestAt(his).done);
        assertFalse(vault.requestAt(hers).done);
        assertEq(vault.nextUnpriced(), slot + 2 * SLOT);

        vm.warp(slot + 2 * SLOT);
        vault.recordValuation();
        assertEq(vault.nextUnpriced(), 0);
        vault.settle(type(uint256).max);
        assertTrue(vault.requestAt(hers).done);
        assertEq(vault.nextUnpriced(), 0);
    }

    function test_next_unpriced_is_still_the_request_a_void_valuation_left_waiting() public {
        uint256 shares = depositAs(vault, alice, 100_000e6);
        deploySpot(90_000e6, 90_000e6, 1e18);
        value(vault);
        setPrice(SPOT, PRICE, 2000);

        atSlot();
        vm.prank(alice);
        uint256 first = vault.requestRedeem(shares / 4, alice);
        skip(SLOT);
        vm.prank(alice);
        uint256 second = vault.requestRedeem(shares / 4, alice);
        uint64 earliest = vault.requestAt(first).earliest;
        assertEq(vault.nextUnpriced(), earliest);

        value(vault);
        value(vault);
        assertTrue(latest().status == ValuationStatus.Void);
        assertGt(latest().time, vault.requestAt(second).earliest);
        assertEq(vault.nextUnpriced(), earliest, "a void valuation priced it");
    }

    function test_next_unpriced_passes_a_request_taken_through_the_fallback() public {
        uint256 shares = depositAs(vault, alice, 100_000e6);
        deploySpot(90_000e6, 90_000e6, 1e18);
        value(vault);
        setPrice(SPOT, PRICE, 2000);

        atSlot();
        vm.prank(alice);
        uint256 first = vault.requestRedeem(shares / 4, alice);
        skip(SLOT);
        vm.prank(alice);
        uint256 second = vault.requestRedeem(shares / 4, alice);
        assertEq(vault.nextUnpriced(), vault.requestAt(first).earliest);

        // Nothing could price them for the fallback delay: the first is
        // taken through the fallback, and the second is the oldest waiting.
        skip(1 days + SLOT);
        vm.prank(alice);
        vault.takeProRata(first);
        assertEq(vault.nextUnpriced(), vault.requestAt(second).earliest);
        vm.prank(alice);
        vault.takeProRata(second);
        assertEq(vault.nextUnpriced(), 0);
    }

    function test_ready_to_close_matches_what_close_accepts() public {
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 50_000e6);
        deploySpot(120_000e6, 100_000e6, 1e18);
        assertFalse(vault.readyToClose(), "ready to close while running");
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);

        // Halted, and the last valuation was before the halt.
        assertFalse(vault.readyToClose());
        vm.expectRevert(IVault.NotYet.selector);
        vault.close();

        // After the halt, and the station still holds the position.
        value(vault);
        assertFalse(vault.readyToClose());
        vm.expectRevert(IVault.NotYet.selector);
        vault.close();

        // Emptied and valued: ready, and the close goes through.
        station.stop();
        station.unwind(1e18);
        value(vault);
        assertTrue(vault.readyToClose());
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);
    }

    function test_close_wants_a_valuation_exactly_when_one_would_let_it_close() public {
        depositAs(vault, alice, 100_000e6);
        deploySpot(90_000e6, 90_000e6, 1e18);
        uint64 slot = atSlot();
        assertFalse(vault.closeWantsValuation(slot + SLOT), "not halted");
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        slot = atSlot();
        assertFalse(vault.closeWantsValuation(slot), "the station still holds the position");

        station.stop();
        station.unwind(1e18);
        assertFalse(vault.closeWantsValuation(vault.haltedAt()), "a valuation at the halt can't close it");

        // Up to a tenth of the minimum deposit left at the station is dust;
        // a unit more is not. Whichever it is, a valuation lets the vault
        // close exactly when it was said to.
        uint256 dust = MINIMUM_DEPOSIT / 10;
        uint256[3] memory gifts = [uint256(0), dust, dust + 1];
        for (uint256 i = 0; i < gifts.length; i++) {
            uint256 snapshot = vm.snapshotState();
            give(address(station), gifts[i]);
            slot = atSlot();
            bool wants = vault.closeWantsValuation(slot);
            assertEq(wants, gifts[i] <= dust);
            vault.recordValuation();
            vault.settle(type(uint256).max);
            assertEq(vault.readyToClose(), wants, "the valuation did otherwise than was said");
            // Once ready, no other valuation is wanted for the close.
            if (wants) assertFalse(vault.closeWantsValuation(slot + SLOT));
            vm.revertToState(snapshot);
        }
    }
}
