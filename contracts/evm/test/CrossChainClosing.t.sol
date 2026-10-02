// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {VaultFactory} from "../src/VaultFactory.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {
    IVault,
    StationPost,
    Transit,
    Valuation,
    ValuationStatus,
    VaultState
} from "../src/interfaces/IVault.sol";
import {ChainName, VaultConfig} from "../src/interfaces/IVaultFactory.sol";
import {Fixture} from "./Fixture.sol";
import {MultiChainFactory} from "./MultiChainFactory.sol";

/// A valuation waiting for posts can always be closed, and a halted vault
/// can't close while money is still on its way between chains.
contract CrossChainClosingTest is Fixture {
    uint256 internal constant UNWINDER_KEY = 0xD1E;
    bytes32 internal constant UNWINDER_IMAGE = keccak256("unwinder image");
    uint256 internal constant OTHER = 999;
    uint64 internal constant DEADLINE = 60;

    address internal unwinder = vm.addr(UNWINDER_KEY);
    Vault internal vault;
    Station internal station;
    bytes32 internal bundle;

    function newFactory(
        VaultFactory.Deps memory deps,
        VaultConfig memory config,
        ChainName[] memory chains
    ) internal override returns (VaultFactory) {
        return new MultiChainFactory(deps, config, chains);
    }

    function setUp() public override {
        super.setUp();
        vm.prank(admin);
        attestation.proposeImage(UNWINDER_IMAGE, ImageKind.Unwinder);
        skip(TIMELOCK);
        blessServiceKey(BUILDER_IMAGE, builder);
        blessServiceKey(REVIEWER_IMAGE, reviewer);

        uint256 id = submitAs(author);
        bundle = keccak256(abi.encode("bundle", id));
        registerTwoChains(id, bundle);
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(bundle, 1));
        vault = activate(id, allowancesCosting(2e6), MINIMUM_TANK);
        station = stationOf(vault);

        blessExecutorKey(executor, address(vault), bundle);
        bytes memory journal = journalFor(UNWINDER_IMAGE, unwinder, leafFor(address(vault), bundle));
        attestation.bless(sealOf(journal), journal, block.chainid, address(vault), bundle, new bytes32[](0));
        vm.prank(executor);
        vault.confirmStations();
        assertTrue(vault.state() == VaultState.Open);
    }

    function registerTwoChains(uint256 id, bytes32 bundleHash) internal {
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

    // Helpers ---------------------------------------------------------------

    function postOf(uint256 mark, uint256 exit) internal pure returns (StationPost memory p) {
        p.mark = mark;
        p.exit = exit;
        p.transit = new Transit[](0);
    }

    /// The next slot a valuation may open in: a full slot after the last
    /// one closed.
    function open() internal returns (uint64 time) {
        uint256 t = vm.getBlockTimestamp() + 2 * SLOT - 1;
        time = uint64(t - (t % SLOT));
        vm.warp(time);
        vm.prank(executor);
        vault.recordValuation();
    }

    function post(uint64 time, StationPost memory p, uint256 key) internal {
        vault.postStation(time, OTHER, p, signed(key, vault.postDigest(time, OTHER, p)));
    }

    function closeAfterDeadline(uint64 time) internal {
        vm.warp(time + DEADLINE + 1);
        vault.closeValuation(time);
        vault.settle(type(uint256).max);
    }

    function valueWith(uint256 mark, uint256 exit) internal {
        uint64 time = open();
        post(time, postOf(mark, exit), EXECUTOR_KEY);
        closeAfterDeadline(time);
    }

    function latest() internal view returns (Valuation memory) {
        return vault.valuationAt(vault.valuationCount() - 1);
    }

    function depositTwo(address who, uint256 amount) internal returns (uint256 shares) {
        uint256 before = vault.balanceOf(who);
        requestDepositAs(vault, who, amount);
        valueWith(0, 0);
        shares = vault.balanceOf(who) - before;
    }

    // Finding 1 (high) ------------------------------------------------------

    /// A post beyond any dollar amount that could exist is refused, so no
    /// stored post can keep a valuation from being added up and closed.
    function test_a_post_too_large_to_add_up_is_refused() public {
        depositTwo(alice, 100_000e6);
        uint64 time = open();
        StationPost memory huge = postOf(type(uint256).max, 0);
        bytes memory sig = signed(EXECUTOR_KEY, vault.postDigest(time, OTHER, huge));
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.postStation(time, OTHER, huge, sig);

        post(time, postOf(10_000e6, 10_000e6), EXECUTOR_KEY);
        closeAfterDeadline(time);
        assertTrue(latest().status == ValuationStatus.Priced);
    }

    // Finding 2 (medium) -----------------------------------------------------

    /// After a kill, money a station reports on its way home keeps the vault
    /// open, even once it counts at zero, so the closed vault's cash is all
    /// of it and every share is paid alike.
    function test_a_halted_vault_cannot_close_while_money_is_on_its_way() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        uint256 bobShares = depositTwo(bob, 100_000e6);

        vm.startPrank(executor);
        station.pull(100_000e6);
        station.bridge("local-bridge", OTHER, 100_000e6, "");
        vm.stopPrank();
        valueWith(100_000e6, 100_000e6);

        // Killed; the other chain's money is sent home over a slow bridge.
        vm.prank(admin);
        vault.kill();
        uint64 sentAt = uint64(vm.getBlockTimestamp()) + 1;
        skip(2 hours);

        uint64 time = open();
        StationPost memory p = postOf(0, 0);
        p.transit = new Transit[](1);
        p.transit[0] = Transit({amount: 100_000e6, sentAt: sentAt});
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);
        vm.expectRevert(IVault.NotYet.selector);
        vault.close();

        // It lands at the home station and is brought into the vault; a
        // valuation with nothing on its way lets it close.
        give(address(station), 100_000e6);
        station.stop();
        station.unwind(1e18);
        valueWith(0, 0);
        vault.close();
        assertTrue(vault.state() == VaultState.Closed);

        vm.prank(alice);
        uint256 alicePaid = vault.redeemClosed(aliceShares, alice);
        vm.prank(bob);
        uint256 bobPaid = vault.redeemClosed(bobShares, bob);
        assertApproxEqRel(alicePaid, bobPaid, 0.001e18, "the same shares were paid differently");
        assertApproxEqRel(alicePaid, 100_000e6, 0.01e18);
    }
}
