// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {
    ClaimState,
    IVault,
    Request,
    StationPost,
    Tank,
    Transit,
    Valuation,
    ValuationStatus,
    VaultState
} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// The cross-chain surface of a two-chain vault: forward pricing across the
/// post window, a payday at a closed valuation, the units' scaling of other
/// stations' posts, money on its way that a second post can't clear, and
/// signatures that one vault or chain can't lend to another.
contract CrossChainPropertiesTest is TwoChainVault {
    uint256 internal constant WAD = 1e18;

    function assertSolvent(string memory when) internal view {
        Tank memory t = vault.tank();
        (, uint256 reserve) = vault.units();
        uint256 booked = vault.cash() + t.balance + vault.escrow() + reserve + vault.heldFor(alice)
            + vault.heldFor(bob) + vault.heldFor(author) + vault.heldFor(platform);
        assertGe(usd.balanceOf(address(vault)), booked, when);
    }

    // Forward pricing inside the post window -----------------------------

    /// A request made while a valuation waits for posts has an `earliest`
    /// after that valuation's time, so the open valuation can't price it even
    /// though it is priced (closed) after the request was made.
    function test_a_request_made_inside_the_post_window_waits_for_the_next_valuation() public {
        depositTwo(alice, 100_000e6);
        uint64 time = open();
        // Thirty seconds into the window, with the posts visible, alice asks
        // to leave.
        vm.warp(time + 30);
        uint256 aliceShares = vault.balanceOf(alice);
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        assertGt(vault.requestAt(id).earliest, time, "a request in the window could target the open slot");

        post(time, postOf(0, 0), EXECUTOR_KEY);
        closeAfterDeadline(time);
        assertFalse(vault.requestAt(id).done, "a request made inside the window was priced by that valuation");
        assertEq(usd.balanceOf(alice), 0);

        valueWith(0, 0);
        assertTrue(vault.requestAt(id).done, "it was not priced by the next valuation");
        assertApproxEqRel(usd.balanceOf(alice), 100_000e6, 0.01e18);
    }

    // Replay --------------------------------------------------------------

    /// A post signed for one vault is refused by another on the same chain,
    /// even for the same time and the same station chain.
    function test_a_post_signed_for_one_vault_is_refused_by_another() public {
        // A second two-chain vault, opened by the same executor key.
        uint256 id = submitAs(author);
        bytes32 b = keccak256(abi.encode("bundle", id));
        registerTwoChains(id, b);
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(b, 1));
        Vault two = activate(id, allowancesCosting(2e6), MINIMUM_TANK);
        blessExecutorKey(executor, address(two), b);
        vm.prank(executor);
        two.confirmStations();

        uint64 time = open();
        vm.prank(executor);
        two.recordValuation();
        StationPost memory p = postOf(1_000_000e6, 1_000_000e6);
        p.asOf = time + SLOT;
        bytes memory forOne = signed(EXECUTOR_KEY, vault.postDigest(time, OTHER, p));
        vm.expectRevert(IVault.NotBlessed.selector);
        two.postStation(time, OTHER, p, forOne);
    }

    /// A carried stop signed on one chain is refused on another: the digest
    /// carries the chain id, so the same station address elsewhere can't be
    /// stopped with it.
    function test_a_stop_signed_for_one_chain_is_refused_on_another() public {
        uint256 home = block.chainid;
        bytes32 digest = station.stopDigest("killed", uint64(block.timestamp));
        bytes memory sig = signed(EXECUTOR_KEY, digest);
        // The same station, seen from a chain where it is not home.
        vm.chainId(home + 1);
        assertFalse(station.isHome());
        vm.expectRevert(Station.NotBlessed.selector);
        station.stop("killed", uint64(block.timestamp), sig);
        vm.chainId(home);
    }

    // Units and the other chain's post -------------------------------------

    /// With fallback units outstanding, the other station's post is counted
    /// for the shares' part only, like the home station's value.
    function test_the_other_stations_post_is_scaled_by_the_units_outstanding() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        depositTwo(bob, 100_000e6);
        vm.startPrank(executor);
        station.pull(150_000e6);
        station.bridge("local-bridge", OTHER, 100_000e6, "");
        vm.stopPrank();
        valueWith(100_000e6, 100_000e6);
        assertApproxEqAbs(vault.totalValue(), 200_000e6, 1e6);

        // The other chain goes quiet: alice takes the fallback for half.
        vm.prank(alice);
        uint256 id = vault.requestRedeem(aliceShares, alice);
        Request memory r = vault.requestAt(id);
        vm.warp(r.earliest + 24 hours + 1);
        vm.prank(alice);
        vault.takeProRata(id);
        (uint256 units,) = vault.units();
        assertApproxEqRel(units, 0.5e18, 0.01e18);
        uint256 cash = vault.cash();

        // The other chain posts again: the vault counts the shares' half of
        // both stations.
        valueWith(100_000e6, 100_000e6);
        Valuation memory v = latest();
        uint256 stations = (50_000e6 + 100_000e6) * (WAD - units) / WAD;
        assertApproxEqAbs(v.gross, cash + stations + 1, 1e6, "the post was not scaled by the units");
        assertApproxEqAbs(v.grossExit, cash + stations + 1, 1e6);
    }

    // Payday at a multi-chain close ---------------------------------------

    /// A valuation priced by `closeValuation` on or after Monday runs payday
    /// on the gross that includes the other chain's post.
    function test_payday_runs_when_a_multi_chain_valuation_is_closed() public {
        depositTwo(alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(90_000e6);
        station.bridge("local-bridge", OTHER, 90_000e6, "");
        vm.stopPrank();
        valueWith(90_000e6, 90_000e6);
        uint256 markBefore = vault.mark();
        uint256 authorBefore = vault.balanceOf(author);

        // A week on (the executor re-attested meanwhile), the other chain
        // reports a 10% gain.
        skip(8 days);
        blessExecutorKey(executor, address(vault), bundle);
        valueWith(99_000e6, 99_000e6);
        Valuation memory v = latest();
        assertEq(v.fee, 0, "the fee was not paid at the close");
        assertGt(vault.balanceOf(author), authorBefore, "the author got no fee shares");
        assertGt(vault.mark(), markBefore, "the mark was not raised");
        // 109k gross, less 15% of the ~9k above the mark: about 1.0765.
        assertApproxEqRel(vault.sharePrice(), 1.0765e6, 0.005e18);
    }

    // Exit costs on the queued path ---------------------------------------

    /// An exit cost reported on a claim the cash covers is taken out of the
    /// payout at the valuation that prices it, and the vault stays solvent.
    function test_an_exit_cost_on_a_queued_claim_is_paid_at_the_valuation() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        depositTwo(bob, 100_000e6);
        vm.prank(executor);
        station.pull(190_000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        valueWith(0, 0);
        assertEq(vault.claimCount(), 1);

        // The money comes home; the claim is queued on the cash.
        vm.prank(executor);
        station.push(190_000e6);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);

        address payee = makeAddr("payee");
        vault.reportExitCost(1, 10e6, payee, signed(UNWINDER_KEY, vault.exitCostDigest(1, 10e6, payee)));

        valueWith(0, 0);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        assertEq(usd.balanceOf(payee), 10e6);
        assertEq(vault.claimAt(1).paid, usd.balanceOf(alice));
        assertApproxEqRel(usd.balanceOf(alice) + 10e6, 100_000e6, 0.01e18);
        assertSolvent("after the queued claim paid its exit cost");
        (uint256 cost,) = vault.exitCostOf(1);
        assertEq(cost, 0);
    }

    // Transit that timed out keeps the vault from closing ------------------

    /// A home send the post keeps naming as in flight past the timeout counts
    /// at zero but still marks money as moving, so a halted vault can't close
    /// on it; once it lands and is brought home the vault closes with all of
    /// it.
    function test_a_timed_out_home_send_still_blocks_the_close_until_it_lands() public {
        uint256 aliceShares = depositTwo(alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(50_000e6);
        station.bridge("local-bridge", OTHER, 50_000e6, "");
        vm.stopPrank();

        vm.prank(admin);
        vault.kill();
        skip(2 hours);
        uint64 time = open();
        StationPost memory p = postOf(0, 0);
        p.inFlightFromHome = new uint64[](1);
        p.inFlightFromHome[0] = 0;
        post(time, p, EXECUTOR_KEY);
        closeAfterDeadline(time);
        Valuation memory v = latest();
        assertEq(v.gross, v.cash + 1, "a timed-out send was counted");
        vm.expectRevert(IVault.NotYet.selector);
        vault.close();

        // It lands back at the home station (the other chain returned it);
        // the stopped station is unwound and the vault closes with it all.
        give(address(station), 50_000e6);
        skip(HALT_GRACE);
        station.stop();
        station.unwind(1e18);
        valueWith(0, 0);
        vault.close();
        vm.prank(alice);
        uint256 paid = vault.redeemClosed(aliceShares, alice);
        assertApproxEqRel(paid, 100_000e6, 0.01e18, "the landed send was not paid out");
    }

    // Two posts: moving is sticky -------------------------------------------

    /// Once any post for a valuation names money on its way, a later post
    /// without it does not clear the mark: the vault still can't close on it.
    function test_a_second_post_cannot_clear_money_on_its_way() public {
        depositTwo(alice, 100_000e6);
        vm.prank(admin);
        vault.kill();
        skip(2 hours);
        uint64 time = open();
        StationPost memory moving = postOf(0, 0);
        moving.transit = new Transit[](1);
        moving.transit[0] = Transit({amount: 1000e6, sentAt: time - 3 hours});
        post(time, moving, EXECUTOR_KEY);
        post(time, postOf(0, 0), UNWINDER_KEY);
        closeAfterDeadline(time);
        vm.expectRevert(IVault.NotYet.selector);
        vault.close();
    }
}
