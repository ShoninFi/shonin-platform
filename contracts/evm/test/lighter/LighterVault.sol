// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {Vault} from "../../src/Vault.sol";
import {
    LighterBoard,
    LighterPosition,
    LighterPrice,
    LighterReport
} from "../../src/drivers/lighter/LighterBoard.sol";
import {LighterDriver} from "../../src/drivers/lighter/LighterDriver.sol";
import {IZkLighter} from "../../src/drivers/lighter/ZkLighter.sol";
import {ChainNeed} from "../../src/interfaces/IStrategyRegistry.sol";
import {Fixture} from "../Fixture.sol";
import {MockZkLighter} from "./MockZkLighter.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A vault whose station trades on Lighter: the station owns the account,
/// registers the executor's key, and closes and withdraws by priority
/// request; its value is what a blessed key posts on the board.
abstract contract LighterVault is Fixture {
    bytes32 internal constant LIGHTER = "lighter";
    uint16 internal constant ETH = 0;
    uint16 internal constant DOLLAR_ASSET = 3;
    /// $2,700.00: two price decimals; four size decimals.
    uint32 internal constant ETH_PX = 270_000;
    bytes internal constant EXECUTOR_PUBKEY =
        hex"0a000000000000000b000000000000000c000000000000000d000000000000000e00000000000000";

    MockZkLighter internal exchange;
    LighterBoard internal board;
    LighterDriver internal lighter;
    Vault internal vault;
    Station internal station;
    uint32 internal spread = 50;

    function setUp() public virtual override {
        super.setUp();
        exchange = new MockZkLighter(IERC20(address(usd)));
        exchange.listMarket(ETH, 4, 2, ETH_PX);
        board = new LighterBoard();
        LighterDriver.Market[] memory markets = new LighterDriver.Market[](1);
        markets[0] = LighterDriver.Market({index: ETH, sizeDecimals: 4, priceDecimals: 2});
        lighter =
            new LighterDriver(IZkLighter(address(exchange)), IERC20(address(usd)), board, 1 hours, 6, markets);
        vm.prank(admin);
        attestation.proposeDriver(LIGHTER, address(lighter));
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
        chains[0].drivers = new bytes32[](1);
        chains[0].drivers[0] = LIGHTER;
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

    /// What the executor's attested image reads, posted: the exchange's
    /// figures from its API, and the station's pending balance and counters
    /// read on-chain at the same block, stamped with that block's time.
    function _post() internal {
        LighterReport memory r = _reading();
        vm.prank(executor);
        board.post(address(station), r);
    }

    function _reading() internal view returns (LighterReport memory r) {
        uint48 account = _master();
        uint16[] memory markets = new uint16[](1);
        markets[0] = ETH;
        r.asOf = uint64(block.timestamp);
        r.accounts = new uint48[](1);
        r.accounts[0] = account;
        int256 equity = exchange.equity(account, markets);
        r.equity = equity > 0 ? uint256(equity) : 0;
        r.pending = exchange.getPendingBalance(address(station), DOLLAR_ASSET);
        r.arrived = station.arrivals();
        (r.sent,) = station.venueFlows(LIGHTER);
        r.available = new uint256[](1);
        int256 cash = exchange.cash(account);
        r.available[0] = cash > 0 ? uint256(cash) : 0;
        int64 held = exchange.size(account, ETH);
        if (held != 0) {
            r.positions = new LighterPosition[](1);
            r.positions[0] = LighterPosition({account: account, market: ETH, size: held});
        }
        uint32 mark = exchange.markOf(ETH);
        r.prices = new LighterPrice[](1);
        r.prices[0] =
            LighterPrice({market: ETH, mark: mark, index: mark, bid: mark - spread, ask: mark + spread});
    }

    function _nextBlock() internal {
        vm.roll(block.number + 1);
        skip(1);
    }

    /// Alice and Bob each put in 100k; the executor moves 150k to Lighter and
    /// goes long 40 ETH ($108k). The vault keeps 50k of cash. Once the
    /// deposit has been credited, a post shows it.
    function _fundAndTrade() internal returns (uint256 aliceShares) {
        aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        vm.startPrank(executor);
        station.approveKey(LIGHTER, executor);
        station.pull(150_000e6);
        station.deposit(LIGHTER, 150_000e6);
        station.approveKey(LIGHTER, executor);
        vm.stopPrank();
        exchange.trade(_master(), ETH, 400_000);
        skip(lighter.DELIVERY_WINDOW());
        _post();
    }

    /// Alice asks for all her shares back and the cash can't pay it: a claim.
    function _aliceClaims(uint256 aliceShares) internal {
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        _post();
        require(vault.claimCount() == 1, "a claim");
    }

    /// Pay the station's pending balance out, as anyone may.
    function _collect() internal {
        uint128 pending = exchange.getPendingBalance(address(station), DOLLAR_ASSET);
        if (pending != 0) exchange.withdrawPendingBalance(address(station), DOLLAR_ASSET, pending);
    }

    /// The staged close carried out, proved by a post read after its settle
    /// delay, its dollars
    /// withdrawn, paid out and delivered.
    function _settleStaged() internal {
        _nextBlock();
        exchange.process();
        skip(lighter.SETTLE_DELAY());
        _post();
        station.settleClose();
        exchange.process();
        _collect();
        _nextBlock();
        station.settleClose();
    }
}
