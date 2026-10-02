// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";

import {VaultFactory} from "../src/VaultFactory.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {
    ClaimState,
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

/// A vault whose strategy also trades on another chain: it opens once its
/// executor says the other station exists, and every valuation waits for
/// that station's post. Posts that disagree price deposits at the highest
/// mark and withdrawals at the lowest exit; a valuation no post arrived for
/// is void and its requests roll on; money between chains counts at par
/// until its timeout; and an unwinder's exit cost comes out of the claim it
/// served.
contract CrossChainTest is Fixture {
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
        assertTrue(vault.state() == VaultState.Activating);

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

    /// Record a valuation at the next slot and return its time.
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
        if (p.asOf == 0) p.asOf = vault.openedAt(vault.valuationCount() - 1) + SLOT;
        vault.postStation(time, OTHER, p, signed(key, vault.postDigest(time, OTHER, p)));
    }

    function closeAfterDeadline(uint64 time) internal {
        vm.warp(time + DEADLINE + 1);
        vault.closeValuation(time);
        vault.settle(type(uint256).max);
    }

    /// A whole valuation with the other station's one post.
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

    // Lifecycle -------------------------------------------------------------

    /// Another vault on the same two chains, activated but not yet opened.
    function activating() internal returns (Vault two, bytes32 b) {
        uint256 id = submitAs(author);
        b = keccak256(abi.encode("bundle", id));
        registerTwoChains(id, b);
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(b, 1));
        two = activate(id, allowancesCosting(2e6), MINIMUM_TANK);
        assertTrue(two.state() == VaultState.Activating);
    }

    function test_a_vault_trading_elsewhere_is_cancelled_if_it_never_opens() public {
        (Vault two,) = activating();
        vm.expectRevert(IVault.NotYet.selector);
        two.cancel();
        skip(3 days);
        uint256 before = usd.balanceOf(author);
        two.cancel();
        assertTrue(two.state() == VaultState.Cancelled);
        assertEq(usd.balanceOf(author) - before, MINIMUM_TANK);
    }

    function test_only_its_executor_confirms_the_stations_and_opens_it() public {
        (Vault two, bytes32 b) = activating();
        vm.expectRevert(IVault.NotBlessed.selector);
        two.confirmStations();
        blessExecutorKey(executor, address(two), b);
        vm.prank(executor);
        two.confirmStations();
        assertTrue(two.state() == VaultState.Open);
    }

    // Posts -----------------------------------------------------------------

    function test_a_valuation_waits_for_the_other_station_and_counts_its_post() public {
        requestDepositAs(vault, alice, 100_000e6);
        uint64 time = open();
        assertTrue(latest().status == ValuationStatus.Open);
        assertEq(vault.settle(type(uint256).max), 0, "an open valuation priced a request");

        post(time, postOf(10_000e6, 9000e6), EXECUTOR_KEY);
        closeAfterDeadline(time);
        Valuation memory v = latest();
        assertTrue(v.status == ValuationStatus.Priced);
        assertEq(v.gross, v.cash + 10_000e6 + 1);
        assertEq(v.grossExit, v.cash + 9000e6 + 1);
        assertGt(vault.balanceOf(alice), 0);
    }

    function test_disagreeing_posts_price_deposits_high_and_fees_and_withdrawals_low() public {
        requestDepositAs(vault, bob, 100_000e6);
        uint256 mark = vault.mark();
        uint64 time = open();
        post(time, postOf(10_000e6, 9000e6), EXECUTOR_KEY);
        post(time, postOf(12_000e6, 8000e6), UNWINDER_KEY);
        post(time, postOf(11_000e6, 8500e6), EXECUTOR_KEY);
        closeAfterDeadline(time);
        Valuation memory v = latest();
        assertEq(v.gross, v.cash + 10_000e6 + 1, "fees were not measured on the lowest mark");
        assertEq(v.grossExit, v.cash + 8000e6 + 1, "withdrawals were not priced at the lowest exit");
        // Bob's deposit pays the highest mark any post carried.
        uint256 top = v.cash + 12_000e6 + 1;
        uint256 basis = (mark * v.supply + 1e30 - 1) / 1e30;
        uint256 feeAtTop = top > basis ? (top - basis) * 1500 / 10_000 : 0;
        vault.settle(type(uint256).max);
        uint256 atHighest = (100_000e6 - 100e6) * v.supply / (top - feeAtTop);
        assertLe(vault.balanceOf(bob), atHighest, "a deposit bought below the highest mark");
        assertApproxEqRel(vault.balanceOf(bob), atHighest, 1e12);
    }

    function test_a_valuation_nobody_posted_for_is_void_and_its_requests_roll_on() public {
        requestDepositAs(vault, alice, 100_000e6);
        uint64 time = open();
        closeAfterDeadline(time);
        assertTrue(latest().status == ValuationStatus.Void);
        assertEq(vault.balanceOf(alice), 0);

        valueWith(0, 0);
        assertTrue(latest().status == ValuationStatus.Priced);
        assertGt(vault.balanceOf(alice), 0, "the request did not roll on to the next valuation");
    }

    function test_only_a_blessed_key_posts_inside_the_deadline_for_a_declared_chain() public {
        uint64 time = open();
        StationPost memory p = postOf(1, 1);
        p.asOf = time + SLOT;

        bytes memory stranger = signed(0xBAD, vault.postDigest(time, OTHER, p));
        vm.expectRevert(IVault.NotBlessed.selector);
        vault.postStation(time, OTHER, p, stranger);

        bytes memory wrongChain = signed(EXECUTOR_KEY, vault.postDigest(time, 1234, p));
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.postStation(time, 1234, p, wrongChain);

        // A post signed for one figure can't carry another.
        bytes memory forOne = signed(EXECUTOR_KEY, vault.postDigest(time, OTHER, p));
        StationPost memory changed = postOf(1_000_000e6, 1);
        changed.asOf = time + SLOT;
        vm.expectRevert(IVault.NotBlessed.selector);
        vault.postStation(time, OTHER, changed, forOne);

        vm.expectRevert(IVault.NotYet.selector);
        vault.closeValuation(time);

        vm.warp(time + DEADLINE + 1);
        vm.expectRevert(IVault.OutsideWindow.selector);
        vault.postStation(time, OTHER, p, forOne);
    }

    function test_an_open_valuation_holds_the_next_until_it_is_closed() public {
        uint64 time = open();
        vm.warp(time + SLOT);
        vm.expectRevert(IVault.Unsettled.selector);
        vm.prank(executor);
        vault.recordValuation();
        vm.warp(time + DEADLINE + 1);
        vault.closeValuation(time);
        open();
    }

    // Transit ---------------------------------------------------------------

    function test_money_in_transit_counts_at_par_and_late_money_prices_nothing() public {
        uint64 time = open();
        StationPost memory p = postOf(0, 0);
        p.transit = new Transit[](1);
        p.transit[0] = Transit({amount: 5000e6, sentAt: time - 30 minutes});
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);
        Valuation memory v = latest();
        assertEq(v.gross, v.cash + 5000e6 + 1, "a send younger than the timeout counts at par");
        assertEq(v.grossExit, v.gross);

        // A send older than the timeout has no price: the valuation is void.
        time = open();
        p.asOf = time + SLOT;
        p.transit = new Transit[](2);
        p.transit[0] = Transit({amount: 5000e6, sentAt: time - 30 minutes});
        p.transit[1] = Transit({amount: 7000e6, sentAt: time - 2 hours});
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);
        assertTrue(latest().status == ValuationStatus.Void, "a valuation counting late money was priced");
    }

    function inFlight(uint64 a) internal pure returns (uint64[] memory list) {
        list = new uint64[](1);
        list[0] = a;
    }

    function test_a_home_send_counts_while_the_post_names_it_in_flight() public {
        depositTwo(alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(5000e6);
        station.bridge("local-bridge", OTHER, 5000e6, "");
        vm.stopPrank();

        // On its way: the post names it, and it is counted.
        uint64 time = open();
        StationPost memory p = postOf(0, 0);
        p.inFlightFromHome = inFlight(0);
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);
        Valuation memory v = latest();
        assertEq(v.gross, v.cash + 5000e6 + 1, "a home send on its way was not counted");

        // Landed: the other station's own value holds it, and it is counted
        // once.
        time = open();
        post(time, postOf(5000e6, 5000e6), EXECUTOR_KEY);
        closeAfterDeadline(time);
        v = latest();
        assertEq(v.gross, v.cash + 5000e6 + 1, "a landed send was counted twice");
    }

    function test_sends_that_land_out_of_order_are_each_counted_once() public {
        depositTwo(alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(8000e6);
        station.bridge("local-bridge", OTHER, 5000e6, "");
        station.bridge("local-bridge", OTHER, 3000e6, "");
        vm.stopPrank();

        // The second landed first: the other station holds its 3000, and the
        // first is still on its way.
        uint64 time = open();
        StationPost memory p = postOf(3000e6, 3000e6);
        p.inFlightFromHome = inFlight(0);
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);
        Valuation memory v = latest();
        assertEq(v.gross, v.cash + 8000e6 + 1, "a send was counted twice or not at all");
    }

    function test_a_post_names_only_earlier_sends_and_each_once() public {
        depositTwo(alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(2000e6);
        station.bridge("local-bridge", OTHER, 1000e6, "");
        vm.stopPrank();
        uint64 time = open();
        // No send leaves home while the valuation waits for posts.
        vm.prank(executor);
        vm.expectRevert(Station.NotAllowed.selector);
        station.bridge("local-bridge", OTHER, 1000e6, "");

        uint64[] memory twice = new uint64[](2);
        uint64[][3] memory refused = [inFlight(1), inFlight(5), twice];
        for (uint256 i = 0; i < refused.length; i++) {
            StationPost memory p = postOf(0, 0);
            p.asOf = time + SLOT;
            p.inFlightFromHome = refused[i];
            bytes memory sig = signed(EXECUTOR_KEY, vault.postDigest(time, OTHER, p));
            vm.expectRevert(IVault.NotAllowed.selector);
            vault.postStation(time, OTHER, p, sig);
        }
    }

    function test_a_station_bridges_only_to_a_chain_the_vault_trades_on() public {
        depositTwo(alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(1000e6);
        vm.expectRevert(Station.NoRoute.selector);
        station.bridge("local-bridge", 1234, 1000e6, "");
        vm.stopPrank();
    }

    // Exit costs ------------------------------------------------------------

    /// Alice's withdrawal becomes a claim: most of the money is at the
    /// station.
    function aClaim() internal returns (uint256 aliceShares) {
        aliceShares = depositTwo(alice, 100_000e6);
        depositTwo(bob, 100_000e6);
        vm.prank(executor);
        station.pull(190_000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        valueWith(0, 0);
        assertEq(vault.claimCount(), 1);
    }

    function test_an_exit_cost_comes_out_of_the_claim_and_goes_to_whoever_the_unwinder_names() public {
        aClaim();
        address payee = makeAddr("payee");
        bytes memory sig = signed(UNWINDER_KEY, vault.exitCostDigest(1, 10e6, payee));
        vault.reportExitCost(1, 10e6, payee, sig);
        (uint256 cost, address payTo) = vault.exitCostOf(1);
        assertEq(cost, 10e6);
        assertEq(payTo, payee);

        // On a vault with stations elsewhere a claim waits for the cash: the
        // strategy brings it home, the claim queues, and the next valuation
        // pays it, less the exit cost.
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Waiting);
        vm.prank(executor);
        station.push(190_000e6);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);
        valueWith(0, 0);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        assertEq(usd.balanceOf(payee), 10e6);
        assertEq(vault.claimAt(1).paid, usd.balanceOf(alice));
        assertApproxEqRel(usd.balanceOf(alice) + 10e6, 100_000e6, 0.01e18);
    }

    function test_an_exit_cost_is_the_unwinders_word_capped_and_only_ever_raised() public {
        aClaim();
        address payee = makeAddr("payee");

        bytes memory byExecutor = signed(EXECUTOR_KEY, vault.exitCostDigest(1, 10e6, payee));
        vm.expectRevert(IVault.NotBlessed.selector);
        vault.reportExitCost(1, 10e6, payee, byExecutor);

        bytes memory tooMuch = signed(UNWINDER_KEY, vault.exitCostDigest(1, MAX_EXIT_COST + 1, payee));
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.reportExitCost(1, MAX_EXIT_COST + 1, payee, tooMuch);

        bytes memory twenty = signed(UNWINDER_KEY, vault.exitCostDigest(1, 20e6, payee));
        bytes memory ten = signed(UNWINDER_KEY, vault.exitCostDigest(1, 10e6, payee));
        vault.reportExitCost(1, 20e6, payee, twenty);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.reportExitCost(1, 10e6, payee, ten);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.reportExitCost(1, 20e6, payee, twenty);

        // A report for another claim, or once the claim is paid, is refused.
        bytes memory other = signed(UNWINDER_KEY, vault.exitCostDigest(2, 10e6, payee));
        vm.expectRevert();
        vault.reportExitCost(2, 10e6, payee, other);
    }
}
