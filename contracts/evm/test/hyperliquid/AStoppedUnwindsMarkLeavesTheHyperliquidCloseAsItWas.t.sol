// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {CloseBand} from "../../src/drivers/CloseBand.sol";
import {Call} from "../../src/interfaces/IDriver.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// A stopped station's unwind marks the band it gives its drivers, and once
/// the band is full counts on past it. The Hyperliquid driver keeps its own
/// pricing: it closes a marked band exactly as the same band unmarked, and
/// any band at or past the full one as the full band, inside the band and
/// at it, with every market answering and beside one that blocks.
contract AStoppedUnwindsMarkLeavesTheHyperliquidCloseAsItWasTest is HyperliquidVault {
    uint32 internal constant OTHER = 9;

    function _close(uint256 fraction, uint16 band) internal view returns (bytes32 calls, bool blocked) {
        Call[] memory list;
        (list, blocked) = hl.close(address(station), fraction, band);
        calls = keccak256(abi.encode(list));
    }

    function _sameAsUnmarked() internal view {
        uint16[6] memory bands = [uint16(0), 100, 400, 3200, 9999, 10_000];
        for (uint256 i = 0; i < bands.length; i++) {
            (bytes32 plain, bool plainBlocked) = _close(1e18, bands[i]);
            (bytes32 marked, bool markedBlocked) = _close(1e18, uint16(CloseBand.STOPPED | bands[i]));
            assertEq(marked, plain, "a marked band ordered other than the same band unmarked");
            assertEq(markedBlocked, plainBlocked, "a marked band blocked other than the same band unmarked");
        }
        (bytes32 full, bool fullBlocked) = _close(1e18, 10_000);
        uint16[4] memory past = [uint16(10_001), 10_004, 10_052, 0x7FFF];
        for (uint256 i = 0; i < past.length; i++) {
            (bytes32 marked, bool markedBlocked) = _close(1e18, uint16(CloseBand.STOPPED | past[i]));
            assertEq(marked, full, "a band past the full one ordered other than the full band");
            assertEq(markedBlocked, fullBlocked);
        }
    }

    function test_a_marked_band_closes_as_the_same_band_unmarked() public {
        _fundAndTrade();
        _stockGas();
        (bytes32 inBand, bool blocked) = _close(1e18, uint16(CloseBand.STOPPED | 100));
        assertFalse(blocked, "the close was blocked");
        assertTrue(inBand != keccak256(abi.encode(new Call[](0))), "nothing was ordered");
        _sameAsUnmarked();
    }

    function test_beside_an_unlisted_perp_a_marked_band_blocks_and_the_full_band_closes_as_before() public {
        _fundAndTrade();
        _stockGas();
        core.listPerp(OTHER, 2, 5000);
        core.trade(address(station), OTHER, 1000, 5000);
        (, bool blocked) = _close(1e18, uint16(CloseBand.STOPPED | 9999));
        assertTrue(blocked, "a marked close inside the band went ahead beside an unlisted perp");
        (, blocked) = _close(1e18, uint16(CloseBand.STOPPED | 10_003));
        assertFalse(blocked, "a marked full-band close was blocked");
        _sameAsUnmarked();
    }
}
