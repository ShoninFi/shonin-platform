// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {HyperCore} from "../../src/drivers/hyperliquid/HyperCore.sol";
import {HyperliquidDriver} from "../../src/drivers/hyperliquid/HyperliquidDriver.sol";
import {Call} from "../../src/interfaces/IDriver.sol";
import {Stage} from "../../src/interfaces/IStation.sol";
import {ClaimState} from "../../src/interfaces/IVault.sol";
import {HyperliquidVault} from "./HyperliquidVault.sol";

/// A vault whose station trades on Hyperliquid, against a HyperCore that
/// acts on CoreWriter's queue only after the block, as the venue does.
contract HyperliquidStationTest is HyperliquidVault {
    // Valuing ---------------------------------------------------------------

    function test_the_account_is_valued_at_mark_and_at_the_bid_from_the_precompiles() public {
        _fundAndTrade();
        (uint256 mark, uint256 exit) = hl.valueOf(address(station));
        (int256 accountValue,) = core.accountValue(address(station));
        assertEq(mark, uint256(accountValue), "the mark is the venue's account value");
        // Long 666,667 lots, ten ticks under the mark at the bid.
        assertEq(mark - exit, 666_667 * 10, "the exit side is the bid's discount on the long");
        assertApproxEqAbs(mark, 190_000e6, 1e6);
    }

    function test_spot_dollars_count_at_par_and_spot_tokens_at_the_lower_of_price_and_bid() public {
        core.creditSpot(address(station), 0, 50e8);
        // 10 HYPE, at $20 with the bid at $19: the 9 above the reserve kept
        // for sends home count.
        core.creditSpot(address(station), HYPE_TOKEN, 10e8);
        core.setSpot(HYPE_PAIR, 20_000_000, 19_000_000, 21_000_000);
        (uint256 mark, uint256 exit) = hl.valueOf(address(station));
        assertEq(mark, 50e6 + 180e6);
        assertEq(exit, 50e6 + 171e6);
    }

    function test_a_perp_the_driver_does_not_list_blocks_every_close_and_counts_nothing_to_a_leaver()
        public
    {
        _fundAndTrade();
        core.listPerp(9, 2, 5000);
        core.trade(address(station), 9, 1000, 5000);
        (, bool blocked) = hl.close(address(station), 1e18, 100);
        assertTrue(blocked, "an unlisted position must block the close");
        assertTrue(station.outsideBand(), "and void the valuation");
        (uint256 mark, uint256 exit) = hl.valueOf(address(station));
        assertGe(mark - exit, 1000 * 5000, "the unlisted notional is off the exit side");
    }

    function test_a_holding_worth_less_than_the_smallest_order_neither_closes_nor_blocks() public {
        _fundAndTrade();
        // Some HYPE kept for sends, on a book far wider than the band.
        core.creditSpot(address(station), HYPE_TOKEN, 0.4e8);
        core.setSpot(HYPE_PAIR, 20_000_000, 15_000_000, 30_000_000);
        (Call[] memory calls, bool blocked) = hl.close(address(station), 0.5e18, 100);
        assertFalse(blocked, "dust must not block");
        assertEq(calls.length, 1, "only the perp is closed");
        // Ten HYPE on the same book does block.
        core.creditSpot(address(station), HYPE_TOKEN, 10e8);
        (, blocked) = hl.close(address(station), 0.5e18, 100);
        assertTrue(blocked);
    }

    function test_hype_kept_for_sends_home_is_never_sold_never_blocks_and_is_worth_nothing() public {
        _fundAndTrade();
        // $12 of HYPE, more than the smallest order, on a book far wider
        // than the band: still the cost of sending home, under the reserve.
        core.creditSpot(address(station), HYPE_TOKEN, 0.6e8);
        core.setSpot(HYPE_PAIR, 20_000_000, 15_000_000, 30_000_000);
        (Call[] memory calls, bool blocked) = hl.close(address(station), 0.5e18, 100);
        assertFalse(blocked, "the reserve never blocks");
        assertEq(calls.length, 1, "only the perp is closed");
        (uint256 mark,) = hl.valueOf(address(station));
        (int256 accountValue,) = core.accountValue(address(station));
        assertEq(mark, uint256(accountValue), "nothing ever sells it, so it is worth nothing to anyone");
        // Nor is it asked of a proof: the perp shrinking is enough.
        int256[] memory before = hl.exposure(address(station));
        core.trade(address(station), ETH, -333_334, ETH_PX);
        (uint256 part, bool known) = hl.closedPart(address(station), before, 0.5e18, uint64(block.timestamp));
        assertTrue(known);
        assertEq(part, 1e18);
        // Above the reserve, HYPE is a holding like any other.
        core.creditSpot(address(station), HYPE_TOKEN, 2e8);
        (, blocked) = hl.close(address(station), 0.5e18, 100);
        assertTrue(blocked, "1.6 HYPE above the reserve, on that book, is outside the band");
    }

    function test_a_mark_off_the_oracle_by_more_than_the_band_blocks_the_close() public {
        _fundAndTrade();
        core.setPerp(ETH, ETH_PX * 103 / 100, ETH_PX, ETH_PX - 10, ETH_PX + 10);
        (, bool blocked) = hl.close(address(station), 0.5e18, 100);
        assertTrue(blocked);
    }

    function test_close_orders_are_reduce_only_ioc_limited_to_the_band_in_five_significant_figures() public {
        _fundAndTrade();
        _stockGas();
        (bytes memory raw) = _onlyCall(hl, 0.5e18);
        (uint32 asset, bool isBuy, uint64 limitPx, uint64 sz, bool reduceOnly, uint8 tif,) =
            abi.decode(_fields(raw), (uint32, bool, uint64, uint64, bool, uint8, uint128));
        assertEq(asset, ETH);
        assertFalse(isBuy, "a long is closed by selling");
        assertTrue(reduceOnly);
        assertEq(tif, HyperCore.TIF_IOC);
        // Half of 666,667 lots, rounded up, at 10^4 per lot in 10^8 units.
        assertEq(sz, 333_334 * 1e4);
        // $2,700 less 1% is $2,673.
        assertEq(limitPx, 2673e8);
    }

    // A claim's close -------------------------------------------------------

    function test_a_claim_is_closed_proved_measured_swept_and_paid_when_delivered() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        _aliceClaims(aliceShares);

        uint256 cashBefore = vault.cash();
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Closing, "the claim waits for its own close");
        assertLt(vault.cash(), cashBefore, "its part of the cash is set aside");
        Stage memory s = station.stage();
        assertGt(s.fraction, 0.45e18);
        assertTrue(s.forVault);
        assertEq(core.queued(), 1, "one closing order is queued");
        // Nothing can be proved in the block it was placed in.
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();

        _nextBlock();
        // Nor before the venue has surely run its orders.
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
        skip(hl.SETTLE_DELAY());
        station.settleClose();
        s = station.stage();
        assertGt(s.sweptBlock, 0, "proved and swept");
        assertApproxEqRel(s.fetched, 95_000e6, 0.01e18, "the fraction's worth, measured");
        assertLe(s.fetched, s.estimate);
        _nextBlock();
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "delivered");
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "paid at delivery, no valuation needed");
        assertApproxEqRel(usd.balanceOf(alice), 100_000e6, 0.01e18);
        // Bob still owns half of a long the size of his share.
        assertApproxEqRel(uint256(int256(core.szi(address(station), ETH))), 333_333, 0.001e18);
    }

    function test_without_hype_to_send_home_a_claims_close_is_blocked_before_anything_is_sold() public {
        uint256 aliceShares = _fundAndTrade();
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "blocked: nothing staged");
        assertEq(core.queued(), 0, "nothing ordered");
        assertFalse(station.outsideBand(), "valuations are not voided for it");
        // Once the executor has stocked HYPE, the close goes ahead.
        _stockGas();
        value(vault);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Closing);
    }

    function test_a_second_try_while_a_close_is_staged_closes_nothing_more() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        vm.expectRevert();
        vault.settleClaim(1);
        assertEq(core.queued(), 1, "still one order");
    }

    function test_a_close_the_venue_did_not_fill_ends_with_nothing_paid_and_the_claim_waits() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        _aliceClaims(aliceShares);
        uint256 cashBefore = vault.cash();
        vault.settleClaim(1);
        // The book runs away before the venue acts: the IOC fills nothing.
        core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX * 98 / 100, ETH_PX + 10);
        _nextBlock();
        (uint256 markBefore,) = station.value();
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
        skip(station.stage().window);
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "ended");
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "the claim waits again");
        assertEq(vault.claimAt(1).shares, aliceShares, "with all its shares");
        assertEq(vault.cash(), cashBefore, "its part of the cash is back");
        assertEq(core.queued(), 0, "nothing was swept");
        (uint256 markAfter,) = station.value();
        assertEq(markAfter, markBefore);
        assertEq(uint256(int256(core.szi(address(station), ETH))), 666_667, "the position is whole");
    }

    function test_a_partly_filled_close_is_measured_only_once_its_window_is_over() public {
        uint256 aliceShares = _fundAndTrade();
        _stockGas();
        _aliceClaims(aliceShares);
        vault.settleClaim(1);
        core.setDepth(100_000);
        _nextBlock();
        skip(hl.SETTLE_DELAY());
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
        assertEq(core.queued(), 0, "nothing asked for");
    }

    // A stopped station ------------------------------------------------------

    /// What the keeper does with a stopped Hyperliquid station: it repeats
    /// the full unwind. The first places the closing orders and reports
    /// itself not done; once the venue has filled them, the next (no sooner
    /// than the driver's spacing after the last move) sends the
    /// freed dollars to HyperEVM and is done at the venue; once they have
    /// landed, the next sends them home to the vault. Then the station holds
    /// nothing and a further unwind sends nothing.
    function test_a_stopped_station_is_emptied_by_repeated_unwinds_placed_swept_then_sent_home() public {
        _fundAndTrade();
        _stockGas();
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();
        uint256 cashBefore = vault.cash();

        (uint256 home, bool blocked) = station.unwind(1e18);
        assertTrue(blocked, "orders were placed: not done");
        // The closing order, and the dollars the account could already spare
        // sent home beside it.
        assertGe(core.queued(), 1);

        _nextBlock();
        assertEq(core.szi(address(station), ETH), 0, "the venue filled the close");
        uint256 sentHome = home;
        _whenMovable();
        (home, blocked) = station.unwind(1e18);
        sentHome += home;
        assertFalse(blocked, "nothing left to order closed");
        assertGt(core.queued(), 0, "the freed dollars are sent home from the account");

        _nextBlock();
        uint256 landed = usd.balanceOf(address(station));
        assertGt(landed, 0, "the account's dollars landed at the station");
        _whenMovable();
        (home, blocked) = station.unwind(1e18);
        sentHome += home;
        assertFalse(blocked);
        assertEq(home, landed, "everything at the station went home");
        assertEq(vault.cash(), cashBefore + sentHome);
        assertGt(sentHome, 185_000e6, "the account's worth came home, less the venue's costs");

        (uint256 mark,) = station.value();
        // Only the HYPE kept to pay for sends home, which is never sold.
        assertLe(mark, 2e6, "the station holds nothing but its HYPE");
        _whenMovable();
        (home, blocked) = station.unwind(1e18);
        assertEq(home, 0);
        assertFalse(blocked);
    }

    // Keys ------------------------------------------------------------------

    function test_approving_a_key_pins_the_balance_mode_then_names_the_agent_with_its_expiry() public {
        core.credit(address(station), 10e6);
        address key = executor;
        station.approveKey(HL, key);
        assertEq(core.queued(), 2);
        _nextBlock();
        assertEq(core.abstraction(address(station)), hl.ABSTRACTION());
        assertEq(core.agentNamed(address(station), keccak256("shonin")), key);
        uint64 until = attestation.blessedUntil(key, block.chainid, address(vault), station.spec().bundleHash);
        assertEq(
            core.lastAgentName(address(station)),
            string.concat("shonin valid_until ", vm.toString(uint256(until) * 1000))
        );
    }

    function test_revoking_hands_the_agent_name_to_an_address_without_a_key() public {
        core.credit(address(station), 10e6);
        station.approveKey(HL, executor);
        _nextBlock();
        (bool ok,) = address(station).call(abi.encodeCall(Station.revokeKey, (HL, executor)));
        assertFalse(ok, "a blessed key is not cut off by a passer-by");
        // The registry revokes the driver: the key goes.
        vm.prank(admin);
        attestation.revokeDriver(HL, address(hl));
        station.revokeKey(HL, executor);
        _nextBlock();
        assertEq(core.agentNamed(address(station), keccak256("shonin")), hl.burnAgent(address(station)));
    }

    // Reclaim ---------------------------------------------------------------

    function test_money_parked_in_a_venue_vault_is_counted_reclaimed_and_ends_trading() public {
        _fundAndTrade();
        vm.prank(executor);
        station.approveKey(HL, executor);
        _nextBlock();
        address venueVault = makeAddr("venue vault");
        (uint256 markBefore,) = station.value();
        uint64 lockedUntil = uint64((block.timestamp + 1 days) * 1000);
        core.parkInVault(address(station), venueVault, 5000e6, lockedUntil);
        (uint256 markParked,) = station.value();
        assertEq(markBefore - markParked, 5000e6, "the venue's account value leaves it out");

        station.reclaim(HL, venueVault);
        (uint256 markCounted,) = station.value();
        assertEq(markCounted, markBefore, "once told, the station counts it");
        assertTrue(station.reclaimed(HL));
        assertEq(station.approvedKey(HL), address(0), "the key is withdrawn");
        vm.expectRevert(Station.NotAllowed.selector);
        station.approveKey(HL, executor);

        // Locked: nothing comes back yet. Unlocked: it does.
        _nextBlock();
        assertEq(core.vaultEquity(address(station), venueVault), 5000e6);
        skip(1 days);
        station.reclaim(HL, venueVault);
        _nextBlock();
        assertEq(core.vaultEquity(address(station), venueVault), 0);
        (uint256 markHome,) = station.value();
        assertEq(markHome, markBefore);
    }

    function test_reclaim_refuses_a_vault_holding_nothing_of_the_station() public {
        vm.expectRevert(Station.NothingHeld.selector);
        station.reclaim(HL, makeAddr("empty"));
    }

    // Helpers ---------------------------------------------------------------

    function _onlyCall(HyperliquidDriver d, uint256 fraction) internal view returns (bytes memory raw) {
        (bytes memory data) = _closeData(d, fraction);
        return data;
    }

    function _closeData(HyperliquidDriver d, uint256 fraction) internal view returns (bytes memory) {
        (Call[] memory calls, bool blocked) = _close(d, fraction);
        require(!blocked && calls.length == 1, "one order");
        bytes memory data = calls[0].data;
        // Strip sendRawAction's selector and its bytes wrapper.
        bytes memory args = new bytes(data.length - 4);
        for (uint256 i = 0; i < args.length; i++) {
            args[i] = data[i + 4];
        }
        return abi.decode(args, (bytes));
    }

    function _close(HyperliquidDriver d, uint256 fraction) internal view returns (Call[] memory, bool) {
        return d.close(address(station), fraction, 100);
    }

    function _fields(bytes memory raw) internal pure returns (bytes memory fields) {
        fields = new bytes(raw.length - 4);
        for (uint256 i = 0; i < fields.length; i++) {
            fields[i] = raw[i + 4];
        }
    }
}
