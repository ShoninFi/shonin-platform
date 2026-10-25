// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {StationFactory} from "../src/StationFactory.sol";
import {StationSpec} from "../src/interfaces/IStation.sol";
import {Fixture} from "./Fixture.sol";

/// A station on another chain is created only if this chain's registry
/// knows every driver it names: one it doesn't could never value, stop or
/// unwind the station.
contract RemoteStationKnownDriversTest is Fixture {
    bytes32 internal constant BUNDLE = keccak256("bundle");
    uint256 internal constant VAULT_CHAIN = 8453;

    StationFactory internal ownStations;
    address internal vaultAddr = makeAddr("vault on chain 8453");

    function setUp() public override {
        super.setUp();
        ownStations = new StationFactory(address(this));
        actAsVaultFactoryFor(address(ownStations));
        ownStations.initialize(address(attestation), address(usd), address(this), address(new Station()));
        bytes32 leaf = keccak256(
            bytes.concat(keccak256(abi.encode(VAULT_CHAIN, vaultAddr, BUNDLE, block.chainid, executor)))
        );
        bytes memory journal = journalFor(EXECUTOR_IMAGE, executor, leaf);
        attestation.bless(
            sealOf(journal), journal, VAULT_CHAIN, vaultAddr, BUNDLE, executor, new bytes32[](0)
        );
    }

    function specWith(bytes32 driverName) internal view returns (StationSpec memory spec) {
        bytes32[] memory drivers = new bytes32[](1);
        drivers[0] = driverName;
        bytes32[] memory bridges = new bytes32[](1);
        bridges[0] = "local-bridge";
        spec = StationSpec({
            vaultChain: VAULT_CHAIN,
            vault: vaultAddr,
            bundleHash: BUNDLE,
            drivers: drivers,
            bridges: bridges,
            chains: new uint256[](0),
            exitBandBps: 100
        });
    }

    function builderSigns(StationSpec memory spec) internal view returns (bytes memory) {
        return signed(
            BUILDER_KEY,
            ownStations.stationDigest(
                spec.vaultChain, spec.bundleHash, spec.drivers, spec.bridges, spec.chains, spec.exitBandBps
            )
        );
    }

    function test_creating_a_station_with_an_unknown_driver_is_refused() public {
        StationSpec memory spec = specWith("not-on-this-chain");
        bytes memory word = builderSigns(spec);
        vm.prank(executor);
        vm.expectRevert(Station.NotApproved.selector);
        ownStations.create(spec, word);

        StationSpec memory known = specWith("local");
        bytes memory signedKnown = builderSigns(known);
        vm.prank(executor);
        ownStations.create(known, signedKnown);
    }
}
