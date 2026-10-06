// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../../src/Station.sol";
import {Vault} from "../../src/Vault.sol";
import {ICoreDepositWallet} from "../../src/drivers/hyperliquid/HyperCore.sol";
import {HyperliquidDriver} from "../../src/drivers/hyperliquid/HyperliquidDriver.sol";
import {ChainNeed} from "../../src/interfaces/IStrategyRegistry.sol";
import {Valuation} from "../../src/interfaces/IVault.sol";
import {Fixture} from "../Fixture.sol";
import {MockDepositWallet, MockHyperCore} from "./MockHyperCore.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A vault whose station trades on Hyperliquid, against a HyperCore that
/// acts on CoreWriter's queue only after the block, as the venue does.
abstract contract HyperliquidVault is Fixture {
    bytes32 internal constant HL = "hyperliquid";
    uint32 internal constant ETH = 4;
    uint8 internal constant ETH_SZ = 4;
    /// $2,700, as a perp price with four size decimals.
    uint64 internal constant ETH_PX = 270_000;
    uint32 internal constant HYPE_PAIR = 5;
    uint64 internal constant HYPE_TOKEN = 7;
    /// What a send to HyperEVM costs, in HYPE's wei: 0.00002 HYPE, as on
    /// testnet.
    uint64 internal constant SEND_FEE = 2000;

    MockHyperCore internal core;
    MockDepositWallet internal wallet;
    HyperliquidDriver internal hl;
    Vault internal vault;
    Station internal station;

    function setUp() public virtual override {
        super.setUp();
        core = new MockHyperCore(usd);
        core.listPerp(ETH, ETH_SZ, ETH_PX);
        // HYPE at $20: two size decimals, eight wei decimals.
        core.listSpot(HYPE_PAIR, HYPE_TOKEN, "HYPE", 2, 8, 20_000_000);
        core.setSendFee(HYPE_TOKEN, SEND_FEE);
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
        // The executor's own moves have landed before anyone closes.
        skip(hl.DELIVERY_WINDOW());
    }

    /// The HYPE the executor keeps on HyperCore to pay for sends home: a
    /// tenth of one, under the driver's reserve.
    function _stockGas() internal {
        core.creditSpot(address(station), HYPE_TOKEN, 0.1e8);
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

    /// Alice asks for all her shares back and the cash can't pay it: a claim.
    function _aliceClaims(uint256 aliceShares) internal {
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        require(vault.claimCount() == 1, "a claim");
    }

    /// Prove the staged close once its settle delay has passed, then
    /// deliver it once its dollars have landed in the block after.
    function _settleStaged() internal {
        _nextBlock();
        skip(hl.SETTLE_DELAY());
        station.settleClose();
        _nextBlock();
        station.settleClose();
    }

    /// What `who`'s shares are worth at the vault's newest valuation, at the
    /// mark.
    function _worthOf(address who) internal view returns (uint256) {
        uint256 count = vault.valuationCount();
        require(count != 0, "no valuation yet");
        Valuation memory v = vault.valuationAt(count - 1);
        return vault.balanceOf(who) * (v.gross - 1) / v.supply;
    }
}
