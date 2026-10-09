// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ICoreDepositWallet} from "../../src/drivers/hyperliquid/HyperCore.sol";
import {HyperliquidDriver} from "../../src/drivers/hyperliquid/HyperliquidDriver.sol";
import {IStation} from "../../src/interfaces/IStation.sol";
import {ChainNeed} from "../../src/interfaces/IStrategyRegistry.sol";
import {Fixture} from "../Fixture.sol";
import {MockDepositWallet, MockHyperCore} from "./MockHyperCore.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A station closes through drivers that all settle in the transaction, or
/// all after it. Beside a delayed venue, a stage would sell a leaver's
/// fraction at a venue that closes at once whatever part of the delayed
/// close then happened, and every try that closed nothing would leave that
/// sale's spread with the holders who stay. So no station mixes the two, and
/// a vault naming both on one chain is never activated.
contract AStationNeverMixesVenuesThatCloseAtOnceWithDelayedOnesTest is Fixture {
    bytes32 internal constant HL = "hyperliquid";
    uint32 internal constant ETH = 4;
    uint64 internal constant ETH_PX = 270_000;
    uint32 internal constant HYPE_PAIR = 5;
    uint64 internal constant HYPE_TOKEN = 7;

    MockHyperCore internal core;
    MockDepositWallet internal wallet;
    HyperliquidDriver internal hl;
    uint256 internal id;

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

        id = submitAs(author);
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
    }

    function test_a_vault_whose_station_would_mix_the_two_is_never_activated() public {
        (bool ok, bytes memory why) = address(this).call(abi.encodeCall(this.activateAndReturn, (id)));
        assertFalse(ok);
        assertEq(bytes4(why), IStation.MixedExits.selector, "refused for mixing the two");
    }

    function activateAndReturn(uint256 strategy) external returns (address) {
        return address(activate(strategy, allowancesCosting(2e6), MINIMUM_TANK));
    }
}
