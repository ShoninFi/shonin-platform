// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {VaultFactory} from "../src/VaultFactory.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {StationPost, Transit, Valuation, VaultState} from "../src/interfaces/IVault.sol";
import {ChainName, VaultConfig} from "../src/interfaces/IVaultFactory.sol";
import {Fixture} from "./Fixture.sol";
import {MultiChainFactory} from "./MultiChainFactory.sol";

/// A vault whose strategy also trades on another chain, opened, for the
/// tests that need one.
contract TwoChainVault is Fixture {
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

    function setUp() public virtual override {
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
        if (p.asOf == 0) p.asOf = vault.openedAt(vault.valuationCount() - 1);
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
}
