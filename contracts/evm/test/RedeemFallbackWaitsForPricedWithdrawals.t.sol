// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {ClaimState, IVault} from "../src/interfaces/IVault.sol";
import {FaultingDriver} from "./FaultingDriverEscape.t.sol";
import {Fixture} from "./Fixture.sol";

/// The redeem fallback waits while a priced valuation's withdrawals are
/// unsettled: they are paid from the cash first, at the price they were
/// given, and the fallback shares what is left.
contract RedeemFallbackWaitsForPricedWithdrawalsTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function test_the_redeem_fallback_waits_while_a_priced_withdrawal_is_unsettled() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        // 190k goes to the station; 10k of cash stays, which is the floor.
        vm.prank(executor);
        station.pull(190_000e6);
        uint256 cash = vault.cash();
        assertApproxEqAbs(cash, 10_000e6, 1e6);

        // A driver that faults is proposed; it lands after the timelock.
        FaultingDriver faulting = new FaultingDriver();
        vm.prank(admin);
        attestation.proposeDriver("local", address(faulting));

        // Bob asks for 8k back; a valuation prices it (payable from cash),
        // and it is left unsettled.
        vm.prank(bob);
        uint256 bobRequest = vault.requestRedeem(bobShares * 8 / 100, bob);
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vault.recordValuation();
        assertFalse(vault.requestAt(bobRequest).done);

        // Alice asks for everything, after that valuation. Then the venue's
        // driver faults for good: no valuation can be recorded.
        vm.prank(alice);
        uint256 aliceRequest = vault.requestRedeem(aliceShares, alice);
        skip(TIMELOCK);
        t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
        vm.expectRevert();
        vault.recordValuation();

        // A day later alice's fallback waits for bob's priced withdrawal.
        vm.prank(alice);
        vm.expectRevert(IVault.NotYet.selector);
        vault.takeProRata(aliceRequest);

        vault.settle(type(uint256).max);
        assertTrue(vault.requestAt(bobRequest).done);
        assertEq(vault.claimCount(), 0, "a withdrawal priced and payable from cash became a claim");
        assertApproxEqRel(usd.balanceOf(bob), 8000e6 * 999 / 1000, 0.001e18, "bob was not paid his priced 8k");

        // Then hers goes through.
        vm.prank(alice);
        vault.takeProRata(aliceRequest);
        assertGt(usd.balanceOf(alice), 0);
    }
}
