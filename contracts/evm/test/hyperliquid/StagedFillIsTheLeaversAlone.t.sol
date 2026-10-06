// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// A claim closed in one transaction pays the leaver what their fraction
/// actually fetched, so a fill below the mark, inside the band, is theirs
/// alone. A staged close holds to the same rule across two transactions: the
/// claim is paid from its own delivery, what the account's dollars grew by,
/// measured at the proof, never at a later valuation's price. Whoever serves
/// the claim may pick its moment (anyone may), and a fill they arrange at the
/// band's edge costs the stayers nothing.
contract StagedFillIsTheLeaversAloneTest is HyperliquidVault {
    function test_a_fill_inside_the_band_on_a_staged_close_is_the_leavers_alone() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        value(vault);
        uint256 bobBefore = _worthOf(bob);
        assertApproxEqAbs(bobBefore, 100_000e6, 100e6, "his half, less the dead shares' and the tank's part");

        _aliceClaims(aliceShares);
        // Staged with the book tight: the order's limit is the oracle less
        // the band (1%).
        vault.settleClaim(1);
        assertEq(core.queued(), 1);

        // Before the venue acts, the bid is pushed 0.9% under the oracle,
        // still inside the band, and the order fills there.
        core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX * 991 / 1000, ETH_PX + 10);
        _nextBlock();
        _spread(ETH_PX, 10);

        // Proved and measured, swept, delivered: paid there and then.
        skip(hl.SETTLE_DELAY());
        station.settleClose();
        _nextBlock();
        station.settleClose();
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);

        value(vault);
        uint256 bobAfter = _worthOf(bob);
        // The transaction fee kept from Alice's exit (about $100) is his;
        // none of her $810 fill is his to pay.
        assertGe(bobAfter + 1e6, bobBefore, "the stayer must not pay for the leaver's fill");
        assertLt(usd.balanceOf(alice), 100_000e6 - 700e6, "the leaver bore her own fill");
    }
}
