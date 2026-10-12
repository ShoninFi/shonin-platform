// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {ICoreDepositWallet} from "../src/drivers/hyperliquid/HyperCore.sol";
import {HyperliquidDriver} from "../src/drivers/hyperliquid/HyperliquidDriver.sol";
import {
    LighterBoard,
    LighterPosition,
    LighterPrice,
    LighterReport
} from "../src/drivers/lighter/LighterBoard.sol";
import {LighterDriver} from "../src/drivers/lighter/LighterDriver.sol";
import {IZkLighter} from "../src/drivers/lighter/ZkLighter.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {Valuation} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";
import {MockDepositWallet, MockHyperCore} from "./hyperliquid/MockHyperCore.sol";
import {MockZkLighter} from "./lighter/MockZkLighter.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A vault whose one station trades on two venues that both act after the
/// transaction, Hyperliquid and Lighter, each against its stand-in: a
/// claim's close is staged at both at once and proved and measured as one.
abstract contract TwoDelayedVenuesVault is Fixture {
    bytes32 internal constant HL = "hyperliquid";
    bytes32 internal constant LIGHTER = "lighter";
    /// ETH on Hyperliquid at $2,700, four size decimals.
    uint32 internal constant HL_ETH = 4;
    uint64 internal constant HL_PX = 270_000;
    uint32 internal constant HYPE_PAIR = 5;
    uint64 internal constant HYPE_TOKEN = 7;
    /// ETH on Lighter at $2,700.00, four size decimals.
    uint16 internal constant LT_ETH = 0;
    uint32 internal constant LT_PX = 270_000;
    uint16 internal constant DOLLAR_ASSET = 3;
    bytes internal constant EXECUTOR_PUBKEY =
        hex"0a000000000000000b000000000000000c000000000000000d000000000000000e00000000000000";

    MockHyperCore internal core;
    HyperliquidDriver internal hl;
    MockZkLighter internal exchange;
    LighterBoard internal board;
    LighterDriver internal lighter;
    Vault internal vault;
    Station internal station;

    function setUp() public virtual override {
        super.setUp();
        core = new MockHyperCore(usd);
        core.listPerp(HL_ETH, 4, HL_PX);
        core.listSpot(HYPE_PAIR, HYPE_TOKEN, "HYPE", 2, 8, 20_000_000);
        core.setSendFee(HYPE_TOKEN, 2000);
        MockDepositWallet wallet = new MockDepositWallet(core, usd);
        uint32[] memory perps = new uint32[](1);
        perps[0] = HL_ETH;
        uint32[] memory pairs = new uint32[](1);
        pairs[0] = HYPE_PAIR;
        hl = new HyperliquidDriver(IERC20(address(usd)), ICoreDepositWallet(address(wallet)), perps, pairs);

        exchange = new MockZkLighter(IERC20(address(usd)));
        exchange.listMarket(LT_ETH, 4, 2, LT_PX);
        board = new LighterBoard();
        LighterDriver.Market[] memory markets = new LighterDriver.Market[](1);
        markets[0] = LighterDriver.Market({index: LT_ETH, sizeDecimals: 4, priceDecimals: 2});
        LighterDriver.Minimum[] memory minimums = new LighterDriver.Minimum[](1);
        minimums[0] = LighterDriver.Minimum({base: 20, quote: 10e6});
        lighter = new LighterDriver(
            IZkLighter(address(exchange)), IERC20(address(usd)), board, 1 hours, 6, markets, minimums
        );

        vm.startPrank(admin);
        attestation.proposeDriver(HL, address(hl));
        attestation.proposeDriver(LIGHTER, address(lighter));
        vm.stopPrank();
        skip(TIMELOCK);

        uint256 id = submitAs(author);
        bytes32 bundle = keccak256(abi.encode("bundle", id));
        _register(id, bundle);
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(bundle, 1));
        vault = activate(id, allowancesCosting(2e6), MINIMUM_TANK);
        blessExecutorKey(executor, address(vault), bundle);
        station = stationOf(vault);
        vm.prank(executor);
        board.setPubKey(EXECUTOR_PUBKEY);
    }

    function _register(uint256 id, bytes32 bundleHash) internal {
        ChainNeed[] memory chains = new ChainNeed[](1);
        chains[0].chain = "base";
        chains[0].drivers = new bytes32[](2);
        chains[0].drivers[0] = HL;
        chains[0].drivers[1] = LIGHTER;
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

    function _master() internal view returns (uint48 account) {
        (account,) = lighter.masterOf(address(station));
    }

    /// The Lighter accounts, posted as the executor's attested image reads
    /// them.
    function _post() internal {
        LighterReport memory r;
        uint48 account = _master();
        uint16[] memory markets = new uint16[](1);
        markets[0] = LT_ETH;
        r.asOf = uint64(block.timestamp);
        r.accounts = new uint48[](1);
        r.accounts[0] = account;
        int256 equity = exchange.equity(account, markets);
        r.equity = equity > 0 ? uint256(equity) : 0;
        r.pending = exchange.getPendingBalance(address(station), DOLLAR_ASSET);
        r.inFlight = exchange.inFlightOf(address(station));
        r.arrived = station.arrivals();
        (r.sent,) = station.venueFlows(LIGHTER);
        r.available = new uint256[](1);
        int256 cash = exchange.cash(account);
        r.available[0] = cash > 0 ? uint256(cash) : 0;
        int64 held = exchange.size(account, LT_ETH);
        if (held != 0) {
            r.positions = new LighterPosition[](1);
            r.positions[0] = LighterPosition({account: account, market: LT_ETH, size: held});
        }
        uint32 mark = exchange.markOf(LT_ETH);
        r.prices = new LighterPrice[](1);
        r.prices[0] = LighterPrice({market: LT_ETH, mark: mark, index: mark, bid: mark - 50, ask: mark + 50});
        vm.prank(executor);
        board.post(address(station), r);
    }

    /// Both venues act on what was queued, after the block.
    function _nextBlock() internal {
        vm.roll(block.number + 1);
        skip(1);
        core.process();
        exchange.process();
    }

    function _hlSpread(uint64 px) internal {
        core.setPerp(HL_ETH, px, px, px - 10, px + 10);
    }

    /// Alice and Bob each put in 100k; the executor moves 90k to each venue
    /// and goes long 16.6667 ETH on Hyperliquid and 20 ETH on Lighter.
    function _fundAndTrade() internal returns (uint256 aliceShares) {
        aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(180_000e6);
        station.deposit(HL, 90_000e6);
        station.approveKey(LIGHTER, executor);
        station.deposit(LIGHTER, 90_000e6);
        station.approveKey(LIGHTER, executor);
        vm.stopPrank();
        core.trade(address(station), HL_ETH, 166_667, HL_PX);
        _hlSpread(HL_PX);
        exchange.trade(_master(), LT_ETH, 200_000);
        // HYPE to send dollars home with.
        core.creditSpot(address(station), HYPE_TOKEN, 0.1e8);
        skip(lighter.DELIVERY_WINDOW());
        _post();
    }

    /// Alice asks for all her shares back and the cash can't pay it: a
    /// claim.
    function _aliceClaims(uint256 aliceShares) internal {
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        _post();
        require(vault.claimCount() == 1, "a claim");
    }

    function _collect() internal {
        uint128 pending = exchange.getPendingBalance(address(station), DOLLAR_ASSET);
        if (pending != 0) exchange.withdrawPendingBalance(address(station), DOLLAR_ASSET, pending);
    }

    function _worthOf(address who) internal view returns (uint256) {
        Valuation memory v = vault.valuationAt(vault.valuationCount() - 1);
        return vault.balanceOf(who) * (v.gross - 1) / v.supply;
    }
}
