// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {Vault} from "../../src/Vault.sol";
import {HyperCore, ICoreDepositWallet} from "../../src/drivers/hyperliquid/HyperCore.sol";
import {HyperliquidDriver} from "../../src/drivers/hyperliquid/HyperliquidDriver.sol";
import {Call} from "../../src/interfaces/IDriver.sol";
import {Stage} from "../../src/interfaces/IStation.sol";
import {ChainNeed} from "../../src/interfaces/IStrategyRegistry.sol";
import {ClaimState} from "../../src/interfaces/IVault.sol";
import {Fixture} from "../Fixture.sol";
import {MockDepositWallet, MockHyperCore} from "./MockHyperCore.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A vault whose station trades on Hyperliquid, against a HyperCore that
/// acts on CoreWriter's queue only after the block, as the venue does.
contract HyperliquidStationTest is Fixture {
    bytes32 internal constant HL = "hyperliquid";
    uint32 internal constant ETH = 4;
    uint8 internal constant ETH_SZ = 4;
    /// $2,700, as a perp price with four size decimals.
    uint64 internal constant ETH_PX = 270_000;
    uint32 internal constant HYPE_PAIR = 5;
    uint64 internal constant HYPE_TOKEN = 7;

    MockHyperCore internal core;
    MockDepositWallet internal wallet;
    HyperliquidDriver internal hl;
    Vault internal vault;
    Station internal station;

    function setUp() public override {
        super.setUp();
        core = new MockHyperCore(usd);
        core.listPerp(ETH, ETH_SZ, ETH_PX);
        // HYPE at $20: two size decimals, eight wei decimals.
        core.listSpot(HYPE_PAIR, HYPE_TOKEN, 2, 8, 20_000_000);
        wallet = new MockDepositWallet(core, usd);
        uint32[] memory perps = new uint32[](1);
        perps[0] = ETH;
        uint32[] memory pairs = new uint32[](1);
        pairs[0] = HYPE_PAIR;
        hl = new HyperliquidDriver(IERC20(address(usd)), ICoreDepositWallet(address(wallet)), perps, pairs);
        vm.prank(admin);
        attestation.proposeDriver(HL, address(hl));
        skip(TIMELOCK);

        uint256 id = submitAs(author);
        bytes32 bundle = keccak256(abi.encode("bundle", id));
        _register(id, bundle);
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(bundle, 1));
        vault = activate(id, allowancesCosting(2e6), MINIMUM_TANK);
        blessExecutorKey(executor, address(vault), bundle);
        station = stationOf(vault);
    }

    function _register(uint256 id, bytes32 bundleHash) internal {
        ChainNeed[] memory chains = new ChainNeed[](1);
        chains[0].chain = "base";
        chains[0].drivers = new bytes32[](1);
        chains[0].drivers[0] = HL;
        bytes32[] memory none = new bytes32[](0);
        bytes32[] memory restricted = new bytes32[](2);
        restricted[0] = "SANCTIONED";
        restricted[1] = "US";
        bytes32 digest = strategies.registrationDigest(
            id, author, keccak256("source"), bundleHash, keccak256("manifest"), chains, none, restricted
        );
        strategies.register(
            id,
            author,
            keccak256("source"),
            bundleHash,
            keccak256("manifest"),
            chains,
            none,
            restricted,
            signed(BUILDER_KEY, digest),
            signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")))
        );
    }

    /// Alice and Bob each put in 100k; the executor moves 190k to HyperCore
    /// and goes long about 180k of ETH on the account.
    function _fundAndTrade() internal returns (uint256 aliceShares) {
        aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(190_000e6);
        station.deposit(HL, 190_000e6);
        vm.stopPrank();
        // 66.6667 ETH at $2,700.
        core.trade(address(station), ETH, 666_667, ETH_PX);
        _spread(ETH_PX, 10);
    }

    function _spread(uint64 px, uint64 halfTicks) internal {
        core.setPerp(ETH, px, px, px - halfTicks, px + halfTicks);
    }

    /// The venue acts on what was queued, after the block.
    function _nextBlock() internal {
        vm.roll(block.number + 1);
        skip(1);
        core.process();
    }

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
        // 10 HYPE, at $20 with the bid at $19.
        core.creditSpot(address(station), HYPE_TOKEN, 10e8);
        core.setSpot(HYPE_PAIR, 20_000_000, 19_000_000, 21_000_000);
        (uint256 mark, uint256 exit) = hl.valueOf(address(station));
        assertEq(mark, 50e6 + 200e6);
        assertEq(exit, 50e6 + 190e6);
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

    function test_a_mark_off_the_oracle_by_more_than_the_band_blocks_the_close() public {
        _fundAndTrade();
        core.setPerp(ETH, ETH_PX * 103 / 100, ETH_PX, ETH_PX - 10, ETH_PX + 10);
        (, bool blocked) = hl.close(address(station), 0.5e18, 100);
        assertTrue(blocked);
    }

    function test_close_orders_are_reduce_only_ioc_limited_to_the_band_in_five_significant_figures() public {
        _fundAndTrade();
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

    function test_a_claim_is_closed_proved_swept_delivered_and_then_paid_from_cash() public {
        uint256 aliceShares = _fundAndTrade();
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);

        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting, "the claim waits for its cash");
        Stage memory s = station.stage();
        assertGt(s.fraction, 0.45e18);
        assertEq(core.queued(), 1, "one closing order is queued");
        // Nothing can be proved in the block it was placed in.
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();

        _nextBlock();
        station.settleClose();
        assertGt(station.stage().sweptBlock, 0, "proved and swept");
        uint256 cashBefore = vault.cash();
        _nextBlock();
        station.settleClose();
        uint256 delivered = vault.cash() - cashBefore;
        assertApproxEqRel(delivered, 95_000e6, 0.01e18, "the fraction's worth came home");
        assertEq(station.stage().stagedAt, 0);

        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued, "the cash now covers it");
        value(vault);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        assertApproxEqRel(usd.balanceOf(alice), 100_000e6, 0.01e18);
        // Bob still owns half of a long the size of his share.
        assertApproxEqRel(uint256(int256(core.szi(address(station), ETH))), 333_333, 0.001e18);
    }

    function test_a_second_try_while_a_close_is_staged_closes_nothing_more() public {
        uint256 aliceShares = _fundAndTrade();
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        vault.settleClaim(1);
        vault.settleClaim(1);
        assertEq(core.queued(), 1, "still one order");
    }

    function test_a_close_the_venue_did_not_fill_moves_nothing_and_lapses() public {
        uint256 aliceShares = _fundAndTrade();
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        vault.settleClaim(1);
        // The book runs away before the venue acts: the IOC fills nothing.
        core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX * 98 / 100, ETH_PX + 10);
        _nextBlock();
        (uint256 markBefore,) = station.value();
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
        skip(station.STAGE_TIMEOUT());
        station.settleClose();
        assertEq(station.stage().stagedAt, 0, "lapsed");
        assertEq(core.queued(), 0, "nothing was swept");
        (uint256 markAfter,) = station.value();
        assertEq(markAfter, markBefore);
        assertEq(uint256(int256(core.szi(address(station), ETH))), 666_667, "the position is whole");
    }

    function test_a_partly_filled_close_is_not_proved_and_sweeps_nothing() public {
        uint256 aliceShares = _fundAndTrade();
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        vault.settleClaim(1);
        core.setDepth(100_000);
        _nextBlock();
        vm.expectRevert(Station.NotYet.selector);
        station.settleClose();
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
