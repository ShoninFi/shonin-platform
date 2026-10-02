// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {VaultFactory} from "../src/VaultFactory.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {IVault, Request, StationPost, Transit, VaultState} from "../src/interfaces/IVault.sol";
import {ChainName, VaultConfig} from "../src/interfaces/IVaultFactory.sol";
import {Fixture} from "./Fixture.sol";
import {MultiChainFactory} from "./MultiChainFactory.sol";

/// On a vault that trades on other chains, with no holding or transaction
/// fee, a small holder's units on money held elsewhere are not served by
/// anyone else, however small their part of the stations looks here.
contract SmallRemoteUnitsKeptFromStrangersTest is Fixture {
    uint256 internal constant OTHER = 999;
    uint64 internal constant DEADLINE = 60;

    Vault internal vault;
    Station internal station;
    bytes32 internal bundle;

    function newFactory(
        VaultFactory.Deps memory deps,
        VaultConfig memory config,
        ChainName[] memory chains
    ) internal override returns (VaultFactory) {
        config.holdingBps = 0;
        config.transactionBps = 0;
        return new MultiChainFactory(deps, config, chains);
    }

    function setUp() public override {
        super.setUp();
        uint256 id = submitAs(author);
        bundle = keccak256(abi.encode("bundle", id));
        ChainNeed[] memory chains = new ChainNeed[](2);
        chains[0].chain = "base";
        chains[0].drivers = new bytes32[](1);
        chains[0].drivers[0] = "local";
        chains[1].chain = "hyperliquid";
        chains[1].drivers = new bytes32[](1);
        chains[1].drivers[0] = "local";
        bytes32[] memory bridges = new bytes32[](1);
        bridges[0] = "local-bridge";
        bytes32[] memory restricted = new bytes32[](0);
        bytes32 digest = strategies.registrationDigest(
            id, author, keccak256("source"), bundle, keccak256("manifest"), chains, bridges, restricted
        );
        strategies.register(
            id,
            author,
            keccak256("source"),
            bundle,
            keccak256("manifest"),
            chains,
            bridges,
            restricted,
            signed(BUILDER_KEY, digest),
            signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")))
        );
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(bundle, 1));
        vault = activate(id, allowancesCosting(2e6), MINIMUM_TANK);
        station = stationOf(vault);
        blessExecutorKey(executor, address(vault), bundle);
        vm.prank(executor);
        vault.confirmStations();
        assertTrue(vault.state() == VaultState.Open);
    }

    function valueWith(uint256 mark, uint256 exit) internal {
        uint256 t = vm.getBlockTimestamp() + 2 * SLOT - 1;
        uint64 time = uint64(t - (t % SLOT));
        vm.warp(time);
        vm.prank(executor);
        vault.recordValuation();
        StationPost memory p;
        p.mark = mark;
        p.exit = exit;
        p.transit = new Transit[](0);
        p.asOf = time;
        vault.postStation(time, OTHER, p, signed(EXECUTOR_KEY, vault.postDigest(time, OTHER, p)));
        vm.warp(time + DEADLINE + 1);
        vault.closeValuation(time);
        vault.settle(type(uint256).max);
    }

    function depositTwo(address who, uint256 amount) internal returns (uint256 shares) {
        uint256 before = vault.balanceOf(who);
        requestDepositAs(vault, who, amount);
        valueWith(0, 0);
        shares = vault.balanceOf(who) - before;
    }

    function test_a_small_holders_remote_units_are_not_served_by_a_stranger() public {
        uint256 aliceShares = depositTwo(alice, 1_000_000e6);
        uint256 bobShares = depositTwo(bob, 10e6);

        // Nearly everything goes to the other chain, and is valued there.
        uint256 cash = vault.cash();
        uint256 floor = ((cash + 1) * 500 + 9999) / 10_000;
        uint256 away = cash - floor;
        vm.startPrank(executor);
        station.pull(away);
        station.bridge("local-bridge", OTHER, away, "");
        vm.stopPrank();
        valueWith(away, away);

        // The other chain goes quiet for a day: both leave through the
        // fallback. Alice holds nearly all of the stations in units, bob
        // about 9.5 dollars of the money on the other chain.
        vm.prank(alice);
        uint256 a = vault.requestRedeem(aliceShares, alice);
        vm.prank(bob);
        uint256 b = vault.requestRedeem(bobShares, bob);
        Request memory r = vault.requestAt(b);
        vm.warp(r.earliest + 24 hours + 1);
        vm.prank(alice);
        vault.takeProRata(a);
        vm.prank(bob);
        vault.takeProRata(b);
        (uint256 bobHeld,) = vault.unitsOf(bob);
        uint256 bobWorth = away * bobHeld / 1e18;
        assertGt(bobWorth, 9e6, "bob's units are not worth about ten dollars");

        // The other chain is back and posts the same figure: a fresh
        // valuation, with the units outstanding scaling the post down to
        // the dead shares' part.
        blessExecutorKey(executor, address(vault), bundle);
        valueWith(away, away);

        // A stranger may not serve bob.
        vm.prank(makeAddr("stranger"));
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.serveProRata(bob);
        (uint256 heldAfter,) = vault.unitsOf(bob);
        assertEq(heldAfter, bobHeld, "bob's units changed");
    }
}
