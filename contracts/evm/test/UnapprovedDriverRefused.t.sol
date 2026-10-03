// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {VaultFactory} from "../src/VaultFactory.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// A strategy naming a driver the registry never approved can't be
/// activated: its station could never be valued, unwound or stopped.
contract UnapprovedDriverRefusedTest is Fixture {
    function readyWithDriver(bytes32 driverName) internal returns (uint256 id, bytes32 bundleHash) {
        id = submitAs(author);
        bundleHash = keccak256(abi.encode("bundle", id));
        ChainNeed[] memory chains = new ChainNeed[](1);
        chains[0].chain = "base";
        chains[0].drivers = new bytes32[](1);
        chains[0].drivers[0] = driverName;
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
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(bundleHash, 1));
    }

    function test_a_strategy_whose_driver_was_never_approved_cannot_be_activated() public {
        assertEq(attestation.tradingDriver("nowhere"), address(0));
        (uint256 id,) = readyWithDriver("nowhere");

        vm.expectRevert(VaultFactory.UnknownDriver.selector);
        factory.restrictedFor(id);

        bytes32 terms = factory.terms();
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        vm.expectRevert(Station.NotApproved.selector);
        factory.activate(id, allowancesCosting(2e6), 500, MINIMUM_TANK, 0, "", terms);
        vm.stopPrank();
    }
}
