// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A served claim owns what its close brought home and its part of the cash
/// as it stood at the close. Nothing that joins or leaves the cash afterwards
/// (a deposit settled in the same slot, cash pushed home, another claim) changes
/// what it is paid, and the stayers' price is unchanged.
contract ClaimOwnsItsCashAtCloseTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;
    address internal carol;
    uint256 internal carolKey;

    function setUp() public override {
        super.setUp();
        (carol, carolKey) = makeAddrAndKey("carol");
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function depositCarol(uint256 amount) internal returns (uint256 shares) {
        bytes memory statement = statementBy(carolKey, address(vault), carol, vault.restrictedText());
        give(carol, amount);
        vm.startPrank(carol);
        usd.approve(address(vault), amount);
        vault.requestDeposit(amount, carol, statement);
        vm.stopPrank();
        value(vault);
        shares = vault.balanceOf(carol);
    }

    /// Pull `pulled` from the vault and put `atVenue` of it in one BTC long
    /// against one BTC short, no spread. The rest sits idle at the station.
    function deploy(uint256 pulled, uint256 atVenue) internal {
        vm.startPrank(executor);
        station.pull(pulled);
        station.approveKey("local", executor);
        station.deposit("local", atVenue);
        OrderRequest[] memory orders = new OrderRequest[](2);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        orders[1] =
            OrderRequest({market: PERP, isBuy: false, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        setPrice(SPOT, PRICE, 0);
    }

    /// What `shares` are worth at exit prices right now, after the
    /// transaction fee: the most a leaver should ever be paid for them.
    function fairFor(uint256 shares) internal view returns (uint256) {
        (, uint256 stationExit) = station.value();
        uint256 owned = (vault.cash() + stationExit) * shares / vault.totalSupply();
        return owned - owned * vault.fees().transactionBps / 10_000;
    }

    /// Three equal holders, 285k of the 300k deployed. Alice's withdrawal
    /// becomes a claim because 15k of cash can't pay her.
    function bookAliceClaim() internal returns (uint256 aliceShares) {
        aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        depositCarol(100_000e6);
        deploy(285_000e6, 285_000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);
        assertTrue(vault.claimAt(1).state == ClaimState.Pending);
    }

    /// A deposit priced in the same slot, ahead of the claim, adds nothing to
    /// what the claim is paid, whoever made it: the claim is paid what its
    /// shares were worth, and the stayers keep their price.
    function test_a_deposit_settled_in_the_same_slot_does_not_change_a_served_claim() public {
        uint256 aliceShares = bookAliceClaim();
        uint256 fair = fairFor(aliceShares);
        assertApproxEqAbs(fair, 100_000e6, 200e6, "alice's shares are worth about 100k");
        uint256 bobBefore = fairFor(vault.balanceOf(bob));

        // Her deposit request goes in ahead of the claim's request.
        requestDepositAs(vault, alice, 100_000e6);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);

        value(vault);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        uint256 paid = vault.claimAt(1).paid;
        emit log_named_uint("alice's shares were worth (after tx fee)", fair);
        emit log_named_uint("her claim was paid", paid);
        emit log_named_uint("bob's shares before", bobBefore);
        emit log_named_uint("bob's shares after", fairFor(vault.balanceOf(bob)));

        // Alice, over both legs: 200k in, claim payout plus new shares out.
        uint256 aliceOut = paid + fairFor(vault.balanceOf(alice));
        emit log_named_uint("alice in", 200_000e6);
        emit log_named_uint("alice out", aliceOut);

        assertLe(paid, fair + 100e6, "a served claim was paid more than its shares were worth");
        assertLe(aliceOut, 200_000e6, "alice left with more than she put in");
        assertGe(fairFor(vault.balanceOf(bob)) + 100e6, bobBefore, "bob paid for alice's move");
    }

    /// Cash the strategy brings home between the close and the next slot is
    /// the stayers': the claim's fraction of the station was already home.
    function test_cash_pushed_home_before_the_claim_is_priced_does_not_change_it() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        depositCarol(100_000e6);
        // 285k at the station: 100k at the venue, 185k idle.
        deploy(285_000e6, 100_000e6);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);
        uint256 fair = fairFor(aliceShares);
        assertApproxEqAbs(fair, 100_000e6, 200e6);

        // Her third is closed: a third of the positions and a third of the
        // idle dollars come home, about 95k.
        uint256 cashBefore = vault.cash();
        vault.settleClaim(1);
        uint256 home = vault.cash() - cashBefore;
        assertApproxEqAbs(home, 95_000e6, 200e6, "her third came home");

        // The executor sends the rest of the idle dollars home.
        uint256 idle = usd.balanceOf(address(station));
        vm.prank(executor);
        station.push(idle);

        value(vault);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        uint256 paid = vault.claimAt(1).paid;
        emit log_named_uint("alice's shares were worth (after tx fee)", fair);
        emit log_named_uint("her claim was paid", paid);
        assertLe(paid, fair + 100e6, "a served claim was paid a share of cash its shares never owned");
    }

    /// Two claims served before either is priced are each paid their own
    /// fraction, whatever order they are priced in.
    function test_two_claims_served_together_are_each_paid_their_own() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        uint256 carolShares = depositCarol(100_000e6);
        deploy(285_000e6, 285_000e6);

        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        vm.prank(bob);
        vault.requestRedeem(bobShares, bob);
        value(vault);
        assertEq(vault.claimCount(), 2);
        uint256 fairAlice = fairFor(aliceShares);
        uint256 fairBob = fairFor(bobShares);
        uint256 carolBefore = fairFor(carolShares);

        vault.settleClaim(1);
        vault.settleClaim(2);
        value(vault);
        assertTrue(vault.claimAt(1).state == ClaimState.Paid);
        assertTrue(vault.claimAt(2).state == ClaimState.Paid);

        uint256 paidAlice = vault.claimAt(1).paid;
        uint256 paidBob = vault.claimAt(2).paid;
        emit log_named_uint("alice fair", fairAlice);
        emit log_named_uint("alice paid", paidAlice);
        emit log_named_uint("bob fair", fairBob);
        emit log_named_uint("bob paid", paidBob);
        emit log_named_uint("carol before", carolBefore);
        emit log_named_uint("carol after", fairFor(carolShares));

        assertGe(paidAlice + 100e6, fairAlice, "alice was paid less than her fraction");
        assertGe(paidBob + 100e6, fairBob, "bob was paid less than his fraction");
    }
}
