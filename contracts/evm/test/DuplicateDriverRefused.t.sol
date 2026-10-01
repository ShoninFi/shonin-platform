// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// A station takes each driver and bridge once: a strategy record naming the
/// same driver twice would count its venue twice and close a fraction of it
/// twice, so activating it is refused.
contract DuplicateDriverRefusedTest is Fixture {
    uint256 internal strategyId;

    function setUp() public override {
        super.setUp();
        strategyId = submitAs(author);
        bytes32 bundle = keccak256(abi.encode("bundle", strategyId));
        registerWithDuplicateDriver(strategyId, bundle);
        vm.prank(reviewer);
        reviews.submitReview(strategyId, reviewFor(bundle, 1));
    }

    /// The fixture's registration, with this chain's drivers as
    /// `["local", "local"]`, signed by the builder as it would be.
    function registerWithDuplicateDriver(uint256 id, bytes32 bundle) internal {
        ChainNeed[] memory chains = new ChainNeed[](1);
        chains[0].chain = "base";
        chains[0].drivers = new bytes32[](2);
        chains[0].drivers[0] = "local";
        chains[0].drivers[1] = "local";
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
    }

    function test_a_strategy_listing_a_driver_twice_cannot_be_activated() public {
        bytes32 terms = factory.terms();
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        vm.expectRevert(Station.NotAllowed.selector);
        factory.activate(strategyId, allowancesCosting(2e6), 500, MINIMUM_TANK, 0, "", terms);
        vm.stopPrank();
    }
}
