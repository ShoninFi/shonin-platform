// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {StationFactory} from "../src/StationFactory.sol";
import {Vault} from "../src/Vault.sol";

import {StationSpec} from "../src/interfaces/IStation.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// What a blessed executor can't choose on its own: a single-chain vault's
/// station has no bridge to send money where nothing counts it, and a
/// station on another chain gets the exit band the builder signed.
contract StationLimitsTest is Fixture {
    bytes32 internal constant BUNDLE = keccak256("bundle");

    /// A single-chain vault's home station gets no bridge, whatever the
    /// record declares, so its cash can't leave for a chain where no station
    /// would count it.
    function test_a_single_chain_vault_has_no_bridge() public {
        uint256 id = submitAs(author);
        bytes32 b = keccak256(abi.encode("bundle", id));
        registerWithBridge(id, b);
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(b, 1));
        Vault vault = activate(id, allowancesCosting(2e6), MINIMUM_TANK);
        blessExecutorKey(executor, address(vault), b);
        Station station = stationOf(vault);

        depositAs(vault, alice, 100_000e6);

        vm.startPrank(executor);
        station.pull(50_000e6);
        vm.expectRevert(Station.NoRoute.selector);
        station.bridge("local-bridge", 999, 50_000e6, "");
        vm.stopPrank();
        value(vault);

        assertApproxEqAbs(vault.totalValue(), 100_000e6, 1e6, "the money stayed where it is counted");
    }

    function registerWithBridge(uint256 id, bytes32 bundleHash) internal {
        ChainNeed[] memory chains = oneChain();
        bytes32[] memory bridges = new bytes32[](1);
        bridges[0] = "local-bridge";
        bytes32[] memory restricted = new bytes32[](2);
        restricted[0] = "SANCTIONED";
        restricted[1] = "US";
        bytes32 digest = strategies.registrationDigest(
            id, author, keccak256("source"), bundleHash, keccak256("manifest"), chains, bridges, restricted
        );
        strategies.register(
            id,
            author,
            keccak256("source"),
            bundleHash,
            keccak256("manifest"),
            chains,
            bridges,
            restricted,
            signed(BUILDER_KEY, digest),
            signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")))
        );
    }

    /// The builder signs the exit band with the rest of a station on another
    /// chain's description: the executor can't create it with another.
    function test_a_remote_stations_exit_band_is_the_one_the_builder_signed() public {
        StationFactory ownStations = new StationFactory(address(this));
        actAsVaultFactoryFor(address(ownStations));
        ownStations.initialize(address(attestation), address(usd), address(this), address(new Station()));
        address vaultAddr = makeAddr("vault on chain 8453");

        bytes32 leaf = keccak256(
            bytes.concat(keccak256(abi.encode(uint256(8453), vaultAddr, BUNDLE, block.chainid, executor)))
        );
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, leaf);
        attestation.bless(sealOf(journal), journal, 8453, vaultAddr, BUNDLE, executor, new bytes32[](0));

        bytes32[] memory drivers = new bytes32[](1);
        drivers[0] = "local";
        bytes32[] memory bridges = new bytes32[](1);
        bridges[0] = "local-bridge";
        bytes memory word = signed(
            BUILDER_KEY, ownStations.stationDigest(8453, BUNDLE, drivers, bridges, new uint256[](0), 100)
        );

        StationSpec memory spec = StationSpec({
            vaultChain: 8453,
            vault: vaultAddr,
            bundleHash: BUNDLE,
            drivers: drivers,
            bridges: bridges,
            chains: new uint256[](0),
            exitBandBps: 1000
        });
        vm.prank(executor);
        vm.expectRevert(StationFactory.NotBlessed.selector);
        ownStations.create(spec, word);

        spec.exitBandBps = 100;
        vm.prank(executor);
        Station remote = Station(ownStations.create(spec, word));
        assertEq(remote.spec().exitBandBps, vaultConfig().exitBandBps);
    }

    /// A station on another chain sends only home, even to a chain the vault
    /// trades on: money between two other chains goes through home.
    function test_a_remote_station_sends_only_home() public {
        StationFactory ownStations = new StationFactory(address(this));
        actAsVaultFactoryFor(address(ownStations));
        ownStations.initialize(address(attestation), address(usd), address(this), address(new Station()));
        address vaultAddr = makeAddr("vault on chain 8453");
        bytes32 leaf = keccak256(
            bytes.concat(keccak256(abi.encode(uint256(8453), vaultAddr, BUNDLE, block.chainid, executor)))
        );
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, leaf);
        attestation.bless(sealOf(journal), journal, 8453, vaultAddr, BUNDLE, executor, new bytes32[](0));

        bytes32[] memory drivers = new bytes32[](1);
        drivers[0] = "local";
        bytes32[] memory bridges = new bytes32[](1);
        bridges[0] = "local-bridge";
        uint256[] memory chains = new uint256[](1);
        chains[0] = 42_161;
        bytes memory word =
            signed(BUILDER_KEY, ownStations.stationDigest(8453, BUNDLE, drivers, bridges, chains, 100));
        StationSpec memory spec = StationSpec({
            vaultChain: 8453,
            vault: vaultAddr,
            bundleHash: BUNDLE,
            drivers: drivers,
            bridges: bridges,
            chains: chains,
            exitBandBps: 100
        });
        vm.prank(executor);
        Station remote = Station(ownStations.create(spec, word));
        give(address(remote), 10_000e6);

        vm.prank(executor);
        vm.expectRevert(Station.NoRoute.selector);
        remote.bridge("local-bridge", 42_161, 5000e6, "");

        vm.prank(executor);
        remote.bridge("local-bridge", 8453, 5000e6, "");
        assertEq(remote.sendCount(), 1);
    }
}
