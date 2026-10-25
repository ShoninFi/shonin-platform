// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalBridge} from "../src/LocalBridge.sol";
import {Station} from "../src/Station.sol";
import {StationFactory} from "../src/StationFactory.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Send, StationSpec} from "../src/interfaces/IStation.sol";
import {Fixture} from "./Fixture.sol";
import {FakeVault} from "./Station.t.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A station on another chain, from this chain's point of view: what a
/// stranger can do to it once it is stopped, and what a blessed key can do
/// before.
contract RemoteStationUnwindTest is Fixture {
    uint256 internal constant VAULT_CHAIN = 8453;
    uint256 internal constant UNWINDER_KEY = 0xD1E;
    bytes32 internal constant UNWINDER_IMAGE = keccak256("unwinder image");
    bytes32 internal constant BUNDLE = keccak256("bundle");

    address internal unwinder = vm.addr(UNWINDER_KEY);
    StationFactory internal ownStations;
    address internal vaultAddr = makeAddr("vault on chain 8453");
    Station internal remote;

    function setUp() public override {
        super.setUp();
        vm.prank(admin);
        attestation.proposeImage(UNWINDER_IMAGE, ImageKind.Unwinder);
        skip(TIMELOCK);
        blessServiceKey(BUILDER_IMAGE, builder);

        ownStations = new StationFactory(address(this));
        actAsVaultFactoryFor(address(ownStations));
        ownStations.initialize(address(attestation), address(usd), address(this), address(new Station()));
        blessFor(EXECUTOR_IMAGE, executor);
        blessFor(UNWINDER_IMAGE, unwinder);

        StationSpec memory spec = remoteSpec();
        bytes memory word = signed(
            BUILDER_KEY,
            ownStations.stationDigest(
                spec.vaultChain, spec.bundleHash, spec.drivers, spec.bridges, spec.chains, spec.exitBandBps
            )
        );
        vm.prank(executor);
        remote = Station(ownStations.create(spec, word));
        assertFalse(remote.isHome());
    }

    function blessFor(bytes32 image, address signer) internal {
        bytes32 leaf = keccak256(
            bytes.concat(keccak256(abi.encode(VAULT_CHAIN, vaultAddr, BUNDLE, block.chainid, signer)))
        );
        bytes memory journal = journalFor(image, signer, leaf);
        attestation.bless(sealOf(journal), journal, VAULT_CHAIN, vaultAddr, BUNDLE, signer, new bytes32[](0));
    }

    function remoteSpec() internal view returns (StationSpec memory spec) {
        bytes32[] memory drivers = new bytes32[](1);
        drivers[0] = "local";
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

    /// Money the bridge delivered here, put to work: 60k idle, 40k at the
    /// venue, half a BTC long.
    function deployed() internal {
        give(address(remote), 100_000e6);
        vm.startPrank(executor);
        remote.approveKey("local", executor);
        remote.deposit("local", 40_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 0.5e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(remote), orders);
        vm.stopPrank();
        (uint256 mark,) = remote.value();
        assertEq(mark, 100_000e6);
    }

    function test_a_stranger_cannot_unwind_a_running_remote_station() public {
        deployed();
        vm.prank(alice);
        vm.expectRevert(Station.NotAllowed.selector);
        remote.unwind(1e18);
        vm.prank(alice);
        vm.expectRevert(Station.NotBlessed.selector);
        remote.bridge("local-bridge", VAULT_CHAIN, 1, "");
    }

    /// Once stopped on a blessed key's word, anyone unwinds all of it, and
    /// every dollar goes over the first declared bridge to the station's own
    /// address on the vault's chain, nowhere else.
    function test_a_stopped_remote_station_is_sent_home_by_anyone() public {
        deployed();
        bytes32 digest = remote.stopDigest("killed", uint64(block.timestamp));
        remote.stop("killed", uint64(block.timestamp), signed(UNWINDER_KEY, digest));
        assertTrue(remote.stopped());
        assertFalse(venue.isAgent(address(remote), executor), "the executor kept its venue key");

        vm.prank(alice);
        vm.expectRevert(Station.NotAllowed.selector);
        remote.unwind(0.5e18);

        uint256 bridgeBefore = usd.balanceOf(address(bridge));
        vm.prank(alice);
        (uint256 sentHome, bool blocked) = remote.unwind(1e18);
        assertFalse(blocked);
        assertEq(sentHome, 100_000e6, "not all of it went home");
        assertEq(usd.balanceOf(address(remote)), 0);
        assertEq(venue.positionOf(address(remote), SPOT).size, 0);
        assertEq(usd.balanceOf(address(bridge)) - bridgeBefore, 100_000e6);
        Send[] memory sends = remote.sends();
        assertEq(sends.length, 1);
        assertEq(sends[0].toChain, VAULT_CHAIN);
        assertEq(sends[0].amount, 100_000e6);

        // Nothing else can be bridged from a stopped station.
        vm.prank(executor);
        vm.expectRevert(Station.AlreadyStopped.selector);
        remote.bridge("local-bridge", VAULT_CHAIN, 1, "");
    }

    /// A stopped remote station whose only bridge the registry has revoked
    /// can't send anything home, since it keeps the bridge it was created
    /// with; the unwind reverts whole, so nothing is sold into a station that
    /// can't move it.
    function test_a_revoked_bridge_leaves_the_unwind_whole() public {
        deployed();
        bytes32 digest = remote.stopDigest("killed", uint64(block.timestamp));
        remote.stop("killed", uint64(block.timestamp), signed(UNWINDER_KEY, digest));
        address pinned = remote.pinnedBridge("local-bridge");
        vm.prank(admin);
        attestation.revokeBridge("local-bridge", pinned);
        vm.prank(alice);
        vm.expectRevert(Station.NotApproved.selector);
        remote.unwind(1e18);
        assertEq(venue.positionOf(address(remote), SPOT).size, 0.5e18, "the position was sold anyway");
    }
}
