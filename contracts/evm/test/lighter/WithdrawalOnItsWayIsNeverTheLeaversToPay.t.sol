// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";

/// Lighter takes a withdrawal out of the account as soon as it carries the
/// request out, and its contract credits the station's pending balance only
/// minutes later, when the batch is executed on-chain. A staged close is
/// measured against what the account held when it was placed, so a
/// withdrawal on its way at the stage or at the proof must be counted on
/// both sides, or the leaver pays for all of it. No close is staged from a
/// post read before the exchange could have carried an ask out, and every
/// post names the dollars carried out and not yet credited.
contract WithdrawalOnItsWayIsNeverTheLeaversToPayTest is LighterVault {
    uint256 internal constant WITHDRAWN = 20_000e6;

    function setUp() public override {
        super.setUp();
        exchange.setCreditLater(true);
    }

    function test_no_close_is_staged_while_a_withdrawal_the_executor_asked_for_may_be_on_its_way() public {
        uint256 aliceShares = _fundAndTrade();
        _aliceClaims(aliceShares);

        vm.prank(executor);
        station.withdraw(LIGHTER, WITHDRAWN);
        _nextBlock();

        vault.settleClaim(1);
        assertTrue(
            vault.claimAt(1).state != ClaimState.Closing,
            "a close was staged seconds after the executor asked the exchange for dollars"
        );
    }

    function test_a_leaver_is_not_charged_a_withdrawal_that_is_on_its_way_at_the_stage_and_the_proof()
        public
    {
        uint256 aliceShares = _fundAndTrade();
        _aliceClaims(aliceShares);
        // The vault: 50k of cash, 150k at Lighter. Alice owns half: 100k.

        // The strategy brings 20k home; the exchange carries it out at once.
        vm.prank(executor);
        station.withdraw(LIGHTER, WITHDRAWN);
        exchange.process();
        // The executor's post once the settle delay has passed names the
        // 20k on their way, and her claim is tried from it.
        skip(lighter.SETTLE_DELAY());
        _post();
        assertEq(exchange.inFlightOf(address(station)), WITHDRAWN, "carried out, not credited");
        _nextBlock();
        vault.settleClaim(1);
        assertEq(uint256(vault.claimAt(1).state), uint256(ClaimState.Closing), "her close is staged");

        // The exchange carries out her orders; the 20k are still not
        // credited at the proof.
        _nextBlock();
        exchange.process();
        skip(lighter.SETTLE_DELAY());
        _post();
        station.settleClose();

        // The batch is executed on-chain: the 20k and what her close
        // fetched are pending, paid out, and her close is delivered.
        exchange.process();
        exchange.credit();
        _collect();
        _nextBlock();
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "the stage has ended");

        // Her half of 200k, less the transaction fee and rounding: what she
        // was paid, and what the shares she still holds a claim for own.
        uint256 left = vault.claimAt(1).state == ClaimState.Paid ? 0 : vault.claimAt(1).shares;
        assertGe(
            usd.balanceOf(alice) + left * 100_000e6 / aliceShares,
            99_000e6,
            "the leaver was paid her part less a withdrawal that was on its way"
        );
    }
}
