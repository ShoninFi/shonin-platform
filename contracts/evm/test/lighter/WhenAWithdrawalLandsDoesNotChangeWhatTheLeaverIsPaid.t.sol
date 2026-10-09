// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../../src/interfaces/IVault.sol";
import {LighterVault} from "./LighterVault.sol";

/// Dollars that reach the station from Lighter during a stage are counted
/// once: the station adds them to the venues' measures, which count them out
/// where the post does, and only then compares the total with the least the
/// close fetched. So whether a withdrawal lands before the proof or after it
/// changes nothing a leaver is paid.
contract WhenAWithdrawalLandsDoesNotChangeWhatTheLeaverIsPaidTest is LighterVault {
    uint256 internal constant WITHDRAWN = 20_000e6;

    function setUp() public override {
        super.setUp();
        exchange.setCreditLater(true);
    }

    function _payout(bool paidOutBeforeTheProof) internal returns (uint256) {
        uint256 aliceShares = _fundAndTrade();
        _aliceClaims(aliceShares);
        vm.prank(executor);
        station.withdraw(LIGHTER, WITHDRAWN);
        exchange.process();
        skip(lighter.SETTLE_DELAY());
        _post();
        _nextBlock();
        vault.settleClaim(1);
        require(vault.claimAt(1).state == ClaimState.Closing, "staged");

        // Her order fills 0.9% under the mark, inside the 1% band; the mark
        // is back where it was by the proof.
        _nextBlock();
        exchange.setMark(ETH, uint32(uint256(ETH_PX) * 991 / 1000));
        exchange.process();
        exchange.setMark(ETH, ETH_PX);
        if (paidOutBeforeTheProof) {
            exchange.credit();
            _collect();
        }
        skip(lighter.SETTLE_DELAY());
        _post();
        station.settleClose();
        exchange.process();
        exchange.credit();
        _collect();
        _nextBlock();
        station.settleClose();
        require(station.stage().stagedAt == 0, "ended");
        uint256 left = vault.claimAt(1).state == ClaimState.Paid ? 0 : vault.claimAt(1).shares;
        return usd.balanceOf(alice) + left * 100_000e6 / aliceShares;
    }

    function test_when_a_withdrawal_lands_does_not_change_what_the_leaver_is_paid() public {
        uint256 start = vm.snapshotState();
        uint256 after_ = _payout(false);
        vm.revertToState(start);
        uint256 before_ = _payout(true);
        emit log_named_decimal_uint("paid, the 20k paid out after the proof", after_, 6);
        emit log_named_decimal_uint("paid, the 20k paid out before the proof", before_, 6);
        assertApproxEqAbs(before_, after_, 5e6, "an arrival during the stage changed the leaver's pay");
    }
}
