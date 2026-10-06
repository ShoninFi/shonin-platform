// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {ClaimState, ValuationStatus} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";
import {StdStorage, stdStorage} from "forge-std/Test.sol";

/// While a close is staged nothing is priced, no fallback is taken and the
/// executor moves no money at the station, which is meant to last the few
/// minutes a close takes. So a close whose dollars the venue won't release
/// (the account's margin still needs them) is refused before anything is
/// sold; and one the venue releases only part of after all delivers that
/// part a delivery window after the ask and ends, and the vault prices its
/// other holders' requests again.
contract UndeliverableCloseDoesNotHoldTheVaultTest is HyperliquidVault {
    using stdStorage for StdStorage;

    /// Alice and Bob each put in 100k; the executor moves 190k to HyperCore
    /// and shorts `lots` of ETH on it.
    function _fundAndShort(int64 lots) internal returns (uint256 aliceShares) {
        aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(190_000e6);
        station.deposit(HL, 190_000e6);
        vm.stopPrank();
        core.trade(address(station), ETH, lots, ETH_PX);
        _spread(ETH_PX, 10);
        skip(hl.DELIVERY_WINDOW());
        _stockGas();
    }

    function _bobAsksForATenth() internal {
        uint256 tenth = vault.balanceOf(bob) / 10;
        vm.prank(bob);
        vault.requestRedeem(tenth, bob);
    }

    function _lastStatus() internal view returns (ValuationStatus) {
        return vault.valuationAt(vault.valuationCount() - 1).status;
    }

    function test_a_close_the_account_could_not_release_is_refused_before_anything_is_sold() public {
        // Short 844.4444 ETH, $2.28m: twelve times the account's $190k. The
        // venue lets an account take out only what it holds above a tenth of
        // its positions' worth: half of it closed frees $95k of worth, of
        // which it could release $76k.
        uint256 aliceShares = _fundAndShort(-8_444_444);
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "refused: nothing staged");
        assertEq(core.queued(), 0, "nothing ordered");
        assertEq(station.stage().stagedAt, 0);

        // The vault goes on pricing everyone else's requests.
        _bobAsksForATenth();
        uint256 bobCash = usd.balanceOf(bob);
        skip(1 minutes);
        value(vault);
        assertTrue(_lastStatus() == ValuationStatus.Priced, "the vault prices its other holders' requests");
        assertGt(usd.balanceOf(bob), bobCash, "Bob is paid");
    }

    function test_a_venue_that_releases_part_delivers_that_part_and_the_vault_prices_again_after_the_window()
        public
    {
        // Four times the account: the close passes the check.
        uint256 aliceShares = _fundAndShort(-2_814_815);
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Closing, "her half of the short is ordered closed");

        // Filled at the mark; then the venue raises its margin to 30% of
        // what stays open before the dollars are asked for, so it can spare
        // only about $76k of the $95k measured.
        _nextBlock();
        stdstore.target(address(core)).sig("marginBps()").checked_write(uint256(3000));
        skip(hl.SETTLE_DELAY());
        station.settleClose();
        uint256 fetched = station.stage().fetched;
        assertApproxEqRel(fetched, 95_000e6, 0.01e18, "her half of the account");
        _nextBlock();
        uint256 landed = usd.balanceOf(address(station));
        assertLt(landed, fetched, "the venue could not release all of it");
        _bobAsksForATenth();

        // Not before its window: then what has landed is delivered.
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
        skip(station.stage().window);
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "the stage has ended");
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "the rest of her claim waits its turn");
        uint256 left = vault.claimAt(1).shares;
        assertApproxEqRel(
            left, aliceShares * (fetched - landed) / fetched, 0.01e18, "the shares nothing came home for"
        );
        assertApproxEqRel(usd.balanceOf(alice), landed * 100_000e6 / fetched, 0.01e18, "paid what landed");

        // The vault prices again at once, and settles Bob's request (paid,
        // or a claim of its own where the cash can't pay it).
        value(vault);
        assertTrue(_lastStatus() == ValuationStatus.Priced, "the vault prices again");
        skip(1 minutes);
        value(vault);
        assertTrue(_lastStatus() == ValuationStatus.Priced);
        assertTrue(vault.requestAt(vault.requestCount()).done, "Bob's request is settled");
    }
}
