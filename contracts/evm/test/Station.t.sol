// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {LocalBridge} from "../src/LocalBridge.sol";
import {LocalDriver} from "../src/LocalDriver.sol";
import {Station} from "../src/Station.sol";
import {StationFactory} from "../src/StationFactory.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../src/interfaces/IDriver.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {IStationVault, Send, StationSpec} from "../src/interfaces/IStation.sol";
import {Fixture, MessageHashUtilsLite} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// Stands in for a vault: it holds cash, hands it to its home station and
/// takes it back.
contract FakeVault is IStationVault {
    IERC20 internal immutable dollar;
    address public station;
    uint64 public haltedAt;
    uint64 public constant haltGrace = 10 minutes;
    uint256 public cash;
    bool public valuing;

    constructor(IERC20 dollar_) {
        dollar = dollar_;
    }

    function setStation(address station_) external {
        station = station_;
    }

    function fund(uint256 amount) external {
        cash += amount;
    }

    function halt() external {
        haltedAt = uint64(block.timestamp);
    }

    function toStation(uint256 amount) external {
        require(msg.sender == station, "not the station");
        cash -= amount;
        dollar.transfer(station, amount);
    }

    function fromStation(uint256 amount) external {
        require(msg.sender == station, "not the station");
        dollar.transferFrom(station, address(this), amount);
        cash += amount;
    }
}

/// A driver whose calls try to take the station's dollars.
contract GreedyDriver is IDriver {
    IERC20 internal immutable dollar;
    address internal immutable thief;
    uint8 internal immutable how;

    constructor(IERC20 dollar_, address thief_, uint8 how_) {
        dollar = dollar_;
        thief = thief_;
        how = how_;
    }

    function name() external pure returns (bytes32) {
        return "greedy";
    }

    function targets() external pure returns (address[] memory) {
        return new address[](0);
    }

    function restricted() external pure returns (bytes32[] memory) {
        return new bytes32[](0);
    }

    function custody() external pure returns (Custody) {
        return Custody.Venue;
    }

    function exitPath() external pure returns (ExitPath) {
        return ExitPath.Contract;
    }

    function valuation() external pure returns (ValuationKind) {
        return ValuationKind.OnChain;
    }

    function approveKey(address, address, uint64) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function revokeKey(address, address) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function deposit(address, uint256 amount) external view returns (Call[] memory calls) {
        calls = new Call[](1);
        if (how == 0) {
            calls[0] = Call(address(dollar), 0, abi.encodeCall(IERC20.transfer, (thief, amount)));
        } else if (how == 1) {
            calls[0] = Call(address(dollar), 0, abi.encodeCall(IERC20.approve, (thief, amount)));
        } else {
            calls[0] = Call(thief, 0, "");
        }
    }

    function withdraw(address, uint256) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function close(address, uint256, uint16) external pure returns (Call[] memory, bool) {
        return (new Call[](0), false);
    }

    function sweep(address, uint256) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    function valueOf(address) external pure returns (uint256, uint256) {
        return (0, 0);
    }
}

contract StationTest is Fixture {
    uint256 internal constant UNWINDER_KEY = 0xD1E;
    uint256 internal constant OTHER_EXECUTOR_KEY = 0xE8ED;
    uint256 internal constant HOME = 31_337;

    address internal unwinderKey = vm.addr(UNWINDER_KEY);
    address internal otherExecutor = vm.addr(OTHER_EXECUTOR_KEY);
    bytes32 internal constant UNWINDER_IMAGE = keccak256("unwinder image");
    bytes32 internal constant BUNDLE = keccak256("bundle");

    StationFactory internal ownStations;
    FakeVault internal vault;
    Station internal station;

    function setUp() public override {
        super.setUp();
        vm.prank(admin);
        attestation.proposeImage(UNWINDER_IMAGE, ImageKind.Unwinder);
        skip(TIMELOCK);
        blessServiceKey(BUILDER_IMAGE, builder);

        ownStations = new StationFactory(address(this));
        ownStations.initialize(address(attestation), address(usd), address(this), address(new Station()));

        vault = new FakeVault(IERC20(address(usd)));
        station = Station(ownStations.createHome(address(vault), BUNDLE, locals(), bridges(), others(), 100));
        vault.setStation(address(station));
        give(address(vault), 100_000e6);
        vault.fund(100_000e6);

        blessExecutorKey(executor, address(vault), BUNDLE);
    }

    function locals() internal pure returns (bytes32[] memory list) {
        list = new bytes32[](1);
        list[0] = "local";
    }

    /// The chain the tests' vault also trades on.
    function others() internal pure returns (uint256[] memory list) {
        list = new uint256[](1);
        list[0] = 8453;
    }

    function bridges() internal pure returns (bytes32[] memory list) {
        list = new bytes32[](1);
        list[0] = "local-bridge";
    }

    /// Bless a key from `image` for a vault on `vaultChain`.
    function blessFor(bytes32 image, address signer, uint256 vaultChain, address forVault) internal {
        bytes32 leaf =
            keccak256(bytes.concat(keccak256(abi.encode(vaultChain, forVault, BUNDLE, block.chainid))));
        bytes memory journal = journalFor(image, signer, leaf);
        attestation.bless(sealOf(journal), journal, vaultChain, forVault, BUNDLE, new bytes32[](0));
    }

    /// The executor's money at the venue, one BTC long against one short.
    function trading() internal {
        vm.startPrank(executor);
        station.pull(100_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 100_000e6);
        OrderRequest[] memory orders = new OrderRequest[](2);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        orders[1] =
            OrderRequest({market: PERP, isBuy: false, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
    }

    // Where ownStations are ----------------------------------------------------

    function test_a_station_is_where_the_factory_says_on_every_chain() public view {
        assertEq(ownStations.stationOf(HOME, address(vault)), address(station));
        assertTrue(ownStations.exists(HOME, address(vault)));
        assertTrue(station.isHome());
        StationSpec memory spec = station.spec();
        assertEq(spec.vault, address(vault));
        assertEq(spec.bundleHash, BUNDLE);
    }

    function test_creating_again_returns_the_same_station() public {
        assertEq(
            ownStations.createHome(address(vault), BUNDLE, locals(), bridges(), others(), 100),
            address(station)
        );
    }

    function test_only_the_vault_factory_creates_a_home_station() public {
        vm.prank(alice);
        vm.expectRevert(StationFactory.NotVaultFactory.selector);
        ownStations.createHome(address(1), BUNDLE, locals(), bridges(), others(), 100);
    }

    function test_only_the_named_key_initialises_the_factory_and_only_once() public {
        StationFactory fresh = new StationFactory(alice);
        vm.expectRevert(StationFactory.NotInitializer.selector);
        fresh.initialize(address(attestation), address(usd), address(this), address(1));
        vm.prank(alice);
        fresh.initialize(address(attestation), address(usd), address(this), address(1));
        vm.prank(alice);
        vm.expectRevert(StationFactory.AlreadyInitialized.selector);
        fresh.initialize(address(attestation), address(usd), address(this), address(2));
    }

    function test_nobody_can_take_over_a_station_or_the_implementation() public {
        StationSpec memory spec = station.spec();
        vm.expectRevert(Station.AlreadyInitialized.selector);
        station.initialize(spec);
        Station implementation = Station(ownStations.implementation());
        vm.expectRevert(Station.AlreadyInitialized.selector);
        implementation.initialize(spec);
    }

    // Keys ------------------------------------------------------------------

    function test_a_blessed_executor_is_approved_at_the_venue() public {
        station.approveKey("local", executor);
        assertTrue(venue.isAgent(address(station), executor));
        assertEq(station.approvedKey("local"), executor);
    }

    function test_an_unblessed_or_unwinder_key_is_never_approved() public {
        vm.expectRevert(Station.NotBlessed.selector);
        station.approveKey("local", alice);
        blessFor(UNWINDER_IMAGE, unwinderKey, HOME, address(vault));
        vm.expectRevert(Station.NotBlessed.selector);
        station.approveKey("local", unwinderKey);
    }

    function test_one_executor_at_a_time() public {
        station.approveKey("local", executor);
        blessExecutorKey(otherExecutor, address(vault), BUNDLE);
        // The new instance takes over itself; a passer-by can't do it for it.
        vm.expectRevert(Station.NotAllowed.selector);
        station.approveKey("local", otherExecutor);
        vm.prank(otherExecutor);
        station.approveKey("local", otherExecutor);
        assertFalse(venue.isAgent(address(station), executor));
        assertTrue(venue.isAgent(address(station), otherExecutor));
    }

    function test_a_passer_by_can_revoke_only_a_key_that_has_lapsed() public {
        station.approveKey("local", executor);
        vm.prank(alice);
        vm.expectRevert(Station.StillBlessed.selector);
        station.revokeKey("local", executor);
        skip(BLESSING_TTL + 1);
        vm.prank(alice);
        station.revokeKey("local", executor);
        assertEq(station.approvedKey("local"), address(0));
        // The venue's own expiry had already passed; the record is clean too.
        assertFalse(venue.isAgent(address(station), executor));
    }

    // Moving money ----------------------------------------------------------

    function test_the_executor_moves_cash_between_the_vault_the_station_and_the_venue() public {
        trading();
        (uint256 mark,) = station.value();
        assertEq(mark, 100_000e6);
        assertEq(vault.cash(), 0);

        // Close everything and bring it all back.
        vm.startPrank(executor);
        OrderRequest[] memory orders = new OrderRequest[](2);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: false, size: 1e18, limitPrice: PRICE, reduceOnly: true});
        orders[1] = OrderRequest({market: PERP, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: true});
        venue.place(address(station), orders);
        station.withdraw("local", 100_000e6);
        station.push(100_000e6);
        vm.stopPrank();
        assertEq(vault.cash(), 100_000e6);
    }

    function test_only_the_executor_moves_money() public {
        vm.prank(alice);
        vm.expectRevert(Station.NotBlessed.selector);
        station.pull(1);
        blessFor(UNWINDER_IMAGE, unwinderKey, HOME, address(vault));
        vm.prank(unwinderKey);
        vm.expectRevert(Station.NotBlessed.selector);
        station.deposit("local", 1);
    }

    function test_only_declared_and_approved_drivers_are_used() public {
        vm.prank(executor);
        vm.expectRevert(Station.NotDeclared.selector);
        station.deposit("hyperliquid", 1);
    }

    function test_money_comes_out_through_a_revoked_driver() public {
        trading();
        vm.prank(executor);
        venue.place(address(station), closingAll());
        vm.prank(admin);
        attestation.revokeDriver("local");

        vm.prank(executor);
        vm.expectRevert(Station.NotApproved.selector);
        station.deposit("local", 1);
        vm.prank(executor);
        station.withdraw("local", 100_000e6);
        assertEq(usd.balanceOf(address(station)), 100_000e6);
    }

    function closingAll() internal pure returns (OrderRequest[] memory orders) {
        orders = new OrderRequest[](2);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: false, size: 1e18, limitPrice: PRICE, reduceOnly: true});
        orders[1] = OrderRequest({market: PERP, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: true});
    }

    // The call checks -------------------------------------------------------

    function test_no_driver_can_send_the_stations_dollars_anywhere() public {
        vm.prank(executor);
        station.pull(1000e6);
        for (uint8 how = 0; how < 3; how++) {
            bytes32 name = bytes32(uint256(keccak256(abi.encode("greedy", how))));
            GreedyDriver greedy = new GreedyDriver(IERC20(address(usd)), alice, how);
            vm.prank(admin);
            attestation.proposeDriver(name, address(greedy));
        }
        skip(TIMELOCK);
        StationSpec memory spec = station.spec();
        spec.vault = address(new FakeVault(IERC20(address(usd))));
        spec.drivers = new bytes32[](3);
        for (uint8 how = 0; how < 3; how++) {
            spec.drivers[how] = bytes32(uint256(keccak256(abi.encode("greedy", how))));
        }
        Station greedyStation =
            Station(ownStations.createHome(spec.vault, BUNDLE, spec.drivers, bridges(), others(), 100));
        give(address(greedyStation), 1000e6);
        blessExecutorKey(executor, spec.vault, BUNDLE);

        for (uint8 how = 0; how < 3; how++) {
            vm.prank(executor);
            vm.expectRevert(Station.BadCall.selector);
            greedyStation.deposit(spec.drivers[how], 1000e6);
        }
        assertEq(usd.balanceOf(address(greedyStation)), 1000e6);
        assertEq(usd.allowance(address(greedyStation), alice), 0);
    }

    function test_a_station_has_no_signature_function() public {
        (bool ok,) =
            address(station).call(abi.encodeWithSignature("isValidSignature(bytes32,bytes)", bytes32(0), ""));
        assertFalse(ok);
    }

    // Unwinding and stopping ------------------------------------------------

    function test_the_vault_unwinds_a_leavers_fraction_and_the_cash_comes_home() public {
        trading();
        vm.prank(address(vault));
        (uint256 sentHome, bool blocked) = station.unwind(0.25e18);
        assertFalse(blocked);
        assertEq(sentHome, 25_000e6);
        assertEq(vault.cash(), 25_000e6);
        assertEq(venue.positionOf(address(station), SPOT).size, 0.75e18);
    }

    function test_nothing_closes_when_a_market_is_outside_the_band() public {
        trading();
        setPrice(SPOT, PRICE, 600);
        vm.prank(address(vault));
        (uint256 sentHome, bool blocked) = station.unwind(0.25e18);
        assertTrue(blocked);
        assertEq(sentHome, 0);
        assertEq(venue.positionOf(address(station), SPOT).size, 1e18);
    }

    function test_a_passer_by_unwinds_only_a_stopped_station_and_only_all_of_it() public {
        trading();
        vm.prank(alice);
        vm.expectRevert(Station.NotAllowed.selector);
        station.unwind(1e18);

        vault.halt();
        vm.expectRevert(Station.NotHalted.selector);
        station.stop();
        skip(10 minutes);
        station.stop();
        assertTrue(station.stopped());
        assertFalse(venue.isAgent(address(station), executor));

        vm.prank(alice);
        vm.expectRevert(Station.NotAllowed.selector);
        station.unwind(0.5e18);
        vm.prank(alice);
        station.unwind(1e18);
        assertEq(vault.cash(), 100_000e6);
    }

    function test_a_stopped_station_moves_nothing_but_home() public {
        vault.halt();
        skip(10 minutes);
        station.stop();
        vm.prank(executor);
        vm.expectRevert(Station.AlreadyStopped.selector);
        station.pull(1);
        vm.expectRevert(Station.AlreadyStopped.selector);
        station.approveKey("local", executor);
    }

    // Other chains ----------------------------------------------------------

    function remoteSpec() internal view returns (StationSpec memory spec) {
        spec = StationSpec({
            vaultChain: 8453,
            vault: address(vault),
            bundleHash: BUNDLE,
            drivers: locals(),
            bridges: bridges(),
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

    function test_a_station_on_another_chain_needs_the_vaults_executor_and_the_builders_word() public {
        StationSpec memory spec = remoteSpec();
        bytes memory word = builderSigns(spec);
        vm.prank(executor);
        vm.expectRevert(StationFactory.NotBlessed.selector);
        ownStations.create(spec, word);

        blessFor(EXECUTOR_IMAGE, executor, 8453, address(vault));
        vm.prank(executor);
        vm.expectRevert(StationFactory.NotBlessed.selector);
        ownStations.create(spec, signed(REVIEWER_KEY, keccak256("anything")));

        vm.prank(executor);
        Station remote = Station(ownStations.create(spec, word));
        assertEq(address(remote), ownStations.stationOf(8453, address(vault)));
        assertFalse(remote.isHome());
    }

    function test_a_bridge_send_goes_to_the_stations_own_address_and_is_recorded() public {
        vm.startPrank(executor);
        station.pull(1000e6);
        station.bridge("local-bridge", 8453, 1000e6, "");
        vm.stopPrank();
        Send[] memory sent = station.sends();
        assertEq(sent.length, 1);
        assertEq(sent[0].toChain, 8453);
        assertEq(sent[0].amount, 1000e6);
        assertEq(usd.balanceOf(address(bridge)), 1000e6);

        vm.prank(executor);
        vm.expectRevert(Station.NotDeclared.selector);
        station.bridge("cctp", 8453, 1, "");
    }

    function test_a_carried_stop_needs_a_key_blessed_for_the_vault() public {
        StationSpec memory spec = remoteSpec();
        blessFor(EXECUTOR_IMAGE, executor, 8453, address(vault));
        bytes memory word = builderSigns(spec);
        vm.prank(executor);
        Station remote = Station(ownStations.create(spec, word));

        bytes32 digest = remote.stopDigest("killed", uint64(block.timestamp));
        vm.expectRevert(Station.NotBlessed.selector);
        remote.stop("killed", uint64(block.timestamp), signed(0xBAD, digest));
        remote.stop("killed", uint64(block.timestamp), signed(EXECUTOR_KEY, digest));
        assertTrue(remote.stopped());
    }
}
