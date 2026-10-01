// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {VaultFactory} from "../src/VaultFactory.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {
    IVault,
    Request,
    StationPost,
    Transit,
    Valuation,
    ValuationStatus,
    VaultState
} from "../src/interfaces/IVault.sol";

import {ChainName, VaultConfig} from "../src/interfaces/IVaultFactory.sol";
import {Fixture} from "./Fixture.sol";
import {MultiChainFactory} from "./MultiChainFactory.sol";

/// The edges of pricing across chains: nothing changes the supply or the
/// units while a valuation waits for posts; a home send made after the home
/// station was valued is counted once; the idle floor holds while a
/// valuation is open or void; and a record naming a chain twice is refused.
contract CrossChainEdgesTest is Fixture {
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
        (vault, station, bundle) = twoChainVault(false);
    }

    /// A vault whose strategy also trades on `hyperliquid`, opened by its
    /// executor. With `twice`, the other chain is listed twice in the record.
    function twoChainVault(bool twice) internal returns (Vault v, Station s, bytes32 b) {
        uint256 id = submitAs(author);
        b = keccak256(abi.encode("bundle", id));
        registerChains(id, b, twice);
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(b, 1));
        v = activate(id, allowancesCosting(2e6), MINIMUM_TANK);
        s = stationOf(v);
        assertTrue(v.state() == VaultState.Activating);

        blessExecutorKey(executor, address(v), b);
        bytes memory journal = journalFor(UNWINDER_IMAGE, unwinder, leafFor(address(v), b));
        attestation.bless(sealOf(journal), journal, block.chainid, address(v), b, new bytes32[](0));
        vm.prank(executor);
        v.confirmStations();
        assertTrue(v.state() == VaultState.Open);
    }

    function registerChains(uint256 id, bytes32 bundleHash, bool twice) internal {
        ChainNeed[] memory chains = new ChainNeed[](twice ? 3 : 2);
        chains[0].chain = "base";
        chains[0].drivers = new bytes32[](1);
        chains[0].drivers[0] = "local";
        for (uint256 i = 1; i < chains.length; i++) {
            chains[i].chain = "hyperliquid";
            chains[i].drivers = new bytes32[](1);
            chains[i].drivers[0] = "local";
        }
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

    /// Record a valuation at the next slot boundary and return its time.
    function open(Vault v) internal returns (uint64 time) {
        uint256 t = vm.getBlockTimestamp();
        time = uint64(t - (t % SLOT) + SLOT);
        vm.warp(time);
        v.recordValuation();
    }

    function post(Vault v, uint64 time, StationPost memory p, uint256 key) internal {
        v.postStation(time, OTHER, p, signed(key, v.postDigest(time, OTHER, p)));
    }

    function closeAfterDeadline(Vault v, uint64 time) internal {
        vm.warp(time + DEADLINE + 1);
        v.closeValuation(time);
        v.settle(type(uint256).max);
    }

    function valueWith(Vault v, uint256 mark, uint256 exit) internal {
        uint64 time = open(v);
        post(v, time, postOf(mark, exit), EXECUTOR_KEY);
        closeAfterDeadline(v, time);
    }

    function latest(Vault v) internal view returns (Valuation memory) {
        return v.valuationAt(v.valuationCount() - 1);
    }

    function depositTwo(Vault v, address who, uint256 amount) internal returns (uint256 shares) {
        uint256 before = v.balanceOf(who);
        requestDepositAs(v, who, amount);
        valueWith(v, 0, 0);
        shares = v.balanceOf(who) - before;
    }

    // 1. Forward pricing: the fallback inside an open valuation ------------

    /// The pro-rata fallback waits while a valuation is open: it would burn
    /// shares and take cash from under a snapshot that is about to price
    /// everyone else.
    function test_the_fallback_waits_while_a_valuation_is_open() public {
        uint256 aliceShares = depositTwo(vault, alice, 100_000e6);
        uint256 bobShares = depositTwo(vault, bob, 100_000e6);
        depositTwo(vault, author, 100_000e6);

        // Alice asks to leave; the other chain then goes quiet for a day, so
        // nothing prices her request.
        vm.prank(alice);
        uint256 aliceRequest = vault.requestRedeem(aliceShares, alice);
        Request memory r = vault.requestAt(aliceRequest);
        vm.warp(r.earliest + 24 hours + 1);

        // Bob asks to leave too, and the next valuation opens for both.
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        uint64 time = open(vault);
        assertTrue(latest(vault).status == ValuationStatus.Open);

        // Inside the post window, the fallback is refused.
        uint256 cashBefore = vault.cash();
        vm.prank(alice);
        vm.expectRevert(IVault.NotYet.selector);
        vault.takeProRata(aliceRequest);

        // The valuation closes and prices both requests at its snapshot.
        post(vault, time, postOf(0, 0), EXECUTOR_KEY);
        uint256 bobBefore = usd.balanceOf(bob);
        closeAfterDeadline(vault, time);
        uint256 bobPaid = usd.balanceOf(bob) - bobBefore;

        // Bob owned a third of the pool; what he is paid can't exceed what
        // his shares were worth, beyond the transaction fee.
        assertLe(
            bobPaid,
            100_000e6,
            "bob was paid more than his shares were worth: the snapshot's gross met a smaller supply"
        );
        assertApproxEqRel(usd.balanceOf(alice), 100_000e6, 0.01e18, "alice was paid at the valuation");
        // And the author's shares still own the third of the cash that is
        // left.
        assertGe(
            vault.cash() + 1000e6, cashBefore - usd.balanceOf(alice) - bobPaid, "the holder who stayed paid"
        );
        assertApproxEqRel(vault.cash(), cashBefore / 3, 0.01e18);
    }

    // 2. Transit: a home send in the valuation's own block -------------------

    /// A send the home station makes after it was valued, even in the same
    /// block, is still in its value and isn't counted again as transit.
    function test_a_home_send_in_the_valuations_own_block_is_counted_once() public {
        depositTwo(vault, alice, 100_000e6);
        vm.prank(executor);
        station.pull(5000e6);

        // The valuation is recorded with 5000 idle at the home station...
        uint64 time = open(vault);
        // ...and in the same block, after it, the executor bridges it out.
        vm.prank(executor);
        station.bridge("local-bridge", OTHER, 5000e6, "");

        // The other station's honest post: nothing held, nothing landed.
        post(vault, time, postOf(0, 0), EXECUTOR_KEY);
        closeAfterDeadline(vault, time);
        Valuation memory v = latest(vault);
        assertEq(v.gross, v.cash + 5000e6 + 1, "the send was counted at home and in transit");
    }

    // 3. The idle floor while a valuation is open -----------------------------

    /// The idle floor counts what the home station took since the last
    /// priced valuation looked at it, so it holds while a valuation waits for
    /// posts, and after one is void.
    function test_the_idle_floor_holds_while_a_valuation_is_open() public {
        depositTwo(vault, alice, 100_000e6);
        uint256 cash = vault.cash();
        // The floor is 5% of value; pull everything above it.
        uint256 floor = ((cash + 1) * 500 + 9999) / 10_000;
        vm.prank(executor);
        station.pull(cash - floor);
        vm.prank(executor);
        vm.expectRevert(IVault.BelowFloor.selector);
        station.pull(1);

        // A valuation opens and waits for the other chain.
        open(vault);
        assertTrue(latest(vault).status == ValuationStatus.Open);

        // The floor still holds: nothing was priced since.
        vm.prank(executor);
        try station.pull(floor / 2) {} catch {}
        assertGe(vault.cash(), floor, "cash fell below the idle floor while the valuation was open");
    }

    // 4. A chain listed twice --------------------------------------------------

    /// A record naming the same chain twice would wait for a post that never
    /// comes, so every valuation would be void: activating it is refused.
    function test_a_record_naming_a_chain_twice_cannot_be_activated() public {
        uint256 id = submitAs(author);
        bytes32 b = keccak256(abi.encode("bundle", id));
        registerChains(id, b, true);
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(b, 1));

        vm.expectRevert(VaultFactory.ChainTwice.selector);
        factory.restrictedFor(id);

        bytes32 terms = factory.terms();
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        vm.expectRevert(VaultFactory.ChainTwice.selector);
        factory.activate(id, allowancesCosting(2e6), 500, MINIMUM_TANK, 0, "", terms);
        vm.stopPrank();
    }
}
