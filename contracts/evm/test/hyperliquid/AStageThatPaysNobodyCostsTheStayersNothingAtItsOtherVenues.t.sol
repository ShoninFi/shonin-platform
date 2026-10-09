// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {Vault} from "../../src/Vault.sol";
import {ICoreDepositWallet} from "../../src/drivers/hyperliquid/HyperCore.sol";
import {HyperliquidDriver} from "../../src/drivers/hyperliquid/HyperliquidDriver.sol";
import {OrderRequest} from "../../src/interfaces/ILocalVenue.sol";
import {ChainNeed} from "../../src/interfaces/IStrategyRegistry.sol";
import {ClaimState, Valuation} from "../../src/interfaces/IVault.sol";
import {Fixture} from "../Fixture.sol";
import {MockDepositWallet, MockHyperCore} from "./MockHyperCore.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A station with a venue that closes in the transaction beside one whose
/// closes settle after it. A claim's stage sells the leaver's fraction at
/// the first venue at once, at its bid, and keeps the dollars for the stage.
/// When the stage then ends having closed nothing at the other venue, the
/// claim is paid nothing: what the stage sold at the first venue under the
/// mark, for her alone, is a cost of her own close, paid with a slice of
/// her shares, and never left for the holders who stay, try after try.
contract AStageThatPaysNobodyCostsTheStayersNothingAtItsOtherVenuesTest is Fixture {
    bytes32 internal constant HL = "hyperliquid";
    uint32 internal constant ETH = 4;
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
        core.listPerp(ETH, 4, ETH_PX);
        core.listSpot(HYPE_PAIR, HYPE_TOKEN, "HYPE", 2, 8, 20_000_000);
        core.setSendFee(HYPE_TOKEN, 2000);
        wallet = new MockDepositWallet(core, usd);
        uint32[] memory perps = new uint32[](1);
        perps[0] = ETH;
        uint32[] memory pairs = new uint32[](1);
        pairs[0] = HYPE_PAIR;
        hl = new HyperliquidDriver(IERC20(address(usd)), ICoreDepositWallet(address(wallet)), perps, pairs);
        vm.prank(admin);
        attestation.proposeDriver(HL, address(hl));
        skip(TIMELOCK);
        give(address(venue), 10_000_000e6);

        uint256 id = submitAs(author);
        bytes32 bundle = keccak256(abi.encode("bundle", id));
        ChainNeed[] memory chains = new ChainNeed[](1);
        chains[0].chain = "base";
        chains[0].drivers = new bytes32[](2);
        chains[0].drivers[0] = "local";
        chains[0].drivers[1] = HL;
        bytes32[] memory none = new bytes32[](0);
        bytes32[] memory restricted = new bytes32[](2);
        restricted[0] = "SANCTIONED";
        restricted[1] = "US";
        bytes32 digest = strategies.registrationDigest(
            id, author, keccak256("source"), bundle, keccak256("manifest"), chains, none, restricted
        );
        strategies.register(
            id,
            author,
            keccak256("source"),
            bundle,
            keccak256("manifest"),
            chains,
            none,
            restricted,
            signed(BUILDER_KEY, digest),
            signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")))
        );
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(bundle, 1));
        vault = activate(id, allowancesCosting(2e6), MINIMUM_TANK);
        blessExecutorKey(executor, address(vault), bundle);
        station = stationOf(vault);
    }

    function _nextBlock() internal {
        vm.roll(vm.getBlockNumber() + 1);
        vm.warp(vm.getBlockTimestamp() + 1);
        core.process();
    }

    function _worthOf(address who) internal view returns (uint256) {
        Valuation memory v = vault.valuationAt(vault.valuationCount() - 1);
        return vault.balanceOf(who) * (v.gross - 1) / v.supply;
    }

    function test_a_stage_that_closes_nothing_at_the_delayed_venue_leaves_no_loss_from_its_other_sale()
        public
    {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);

        // 100k to the venue that closes in the transaction, 90k of it in
        // spot; 90k to Hyperliquid, long about 80k of ETH.
        station.approveKey("local", executor);
        vm.startPrank(executor);
        station.pull(190_000e6);
        station.deposit("local", 100_000e6);
        station.deposit(HL, 90_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1.5e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        core.trade(address(station), ETH, 296_296, ETH_PX);
        core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX - 10, ETH_PX + 10);
        core.creditSpot(address(station), HYPE_TOKEN, 0.1e8);
        // The first venue's bid sits 0.9% under its mark, inside the band.
        setPrice(SPOT, PRICE, 180);
        skip(hl.DELIVERY_WINDOW());

        value(vault);
        uint256 bobBefore = _worthOf(bob);

        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1, "a claim");

        for (uint256 round = 0; round < 3; round++) {
            vault.settleClaim(1);
            assertTrue(vault.claimAt(1).state == ClaimState.Closing, "her close is staged");
            // Hyperliquid's bid falls under the order's limit before the
            // venue acts: nothing fills there.
            core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX * 985 / 1000, ETH_PX + 10);
            _nextBlock();
            core.setPerp(ETH, ETH_PX, ETH_PX, ETH_PX - 10, ETH_PX + 10);
            assertEq(core.szi(address(station), ETH), 296_296, "nothing was sold on Hyperliquid");

            vm.warp(vm.getBlockTimestamp() + station.stage().window);
            station.settleClose();
            assertEq(station.stage().stagedAt, 0, "the stage has ended");
            assertEq(usd.balanceOf(alice), 0, "she was paid nothing");
            uint256 left = vault.claimAt(1).shares;
            assertLt(left, aliceShares, "her own sale's slippage is paid with her shares");
            // At most the band on what was sold for her: 1% of 45k.
            assertGt(left * 1000, aliceShares * 995, "and no more than the slippage");
            aliceShares = left;
            value(vault);
        }

        emit log_named_decimal_uint("the stayer before", bobBefore, 6);
        emit log_named_decimal_uint("the stayer after", _worthOf(bob), 6);
        // Twenty dollars cover the holding fee and rounding meanwhile.
        assertGe(_worthOf(bob) + 20e6, bobBefore, "the stayer paid for sales that paid nobody");
    }
}
