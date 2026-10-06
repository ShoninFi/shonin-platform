// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// A claim's close is proved piece by piece: every piece it would order is
/// asked of the proof. If one of them can't be ordered now (the strategy's
/// own resting orders hold it), selling the others would leave a stage that
/// proves nothing, with the leaver's sales made for nothing. So a claim's
/// close that can't order every piece sells nothing anywhere.
contract AStageSellsNothingUnlessItCanOrderEveryPieceTest is HyperliquidVault {
    function test_a_claims_close_whose_spot_piece_is_held_by_resting_orders_sells_nothing() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        // Three HYPE more, two of them above the reserve ($40 at $20); the
        // strategy's own resting sells hold all but a fifth of one.
        core.creditSpot(address(station), HYPE_TOKEN, 3e8);
        core.setSpotHold(address(station), HYPE_TOKEN, 2.9e8);
        _aliceClaims(aliceShares);

        uint256 queued = core.queued();
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state != ClaimState.Closing, "a close was staged");
        assertEq(station.stage().stagedAt, 0, "nothing staged");
        assertEq(core.queued(), queued, "nothing was ordered anywhere");
        _nextBlock();
        assertEq(core.szi(address(station), ETH), 666_667, "the perp was not sold");
    }

    function test_the_same_close_is_staged_once_the_piece_can_be_ordered() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        core.creditSpot(address(station), HYPE_TOKEN, 3e8);
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        assertEq(uint256(vault.claimAt(1).state), uint256(ClaimState.Closing), "staged");
    }
}
