// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Claim, ClaimState, Tank, VaultState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// What the vault promises: it works out its own value, it rounds against
/// whoever is moving, it lets anyone leave, and nothing anybody runs can move
/// a dollar anywhere but back to the people who own it.
contract VaultTest is Fixture {
    bytes32 internal constant STRATEGY = "funding-basis";
    // Named here rather than read from the vault in each call: reading them is
    // itself a call, and it would swallow the prank meant for the next one.
    bytes32 internal constant IDLE = "idle";
    bytes32 internal constant VENUE = "venue";
    bytes32 internal constant VENUE_ID = "local";

    Vault internal vault;
    bytes32 internal bundle;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault(STRATEGY, 1);
        blessExecutorKey(executor, address(vault), bundle);
        vault.approveKey(VENUE_ID, executor);
    }

    /// Put `amount` to work on the venue: long spot, short the perp, which is
    /// the shape the first strategy trades.
    function deployCapital(uint256 amount, uint256 units) internal {
        vm.prank(executor);
        vault.moveCapital(IDLE, VENUE, amount);

        OrderRequest[] memory orders = new OrderRequest[](2);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: units, limitPrice: PRICE * 2, reduceOnly: false});
        orders[1] = OrderRequest({market: PERP, isBuy: false, size: units, limitPrice: 1, reduceOnly: false});
        vm.prank(executor);
        venue.place(address(vault), orders);
    }

    // Shares and value ------------------------------------------------------

    function test_a_share_starts_life_worth_a_dollar() public {
        assertEq(vault.sharePrice(), 1e6);
        uint256 shares = depositAs(vault, alice, 1000e6);
        // A tenth of a per cent goes to the pool, so the shares are worth
        // slightly less than what went in.
        assertEq(shares, 999e18);
        assertEq(vault.totalValue(), 1000e6);
    }

    function test_the_transaction_fee_stays_with_the_people_who_stayed() public {
        depositAs(vault, alice, 1000e6);
        uint256 before = vault.sharePrice();
        depositAs(vault, bob, 1000e6);
        // Bob's fee lifts the value of Alice's shares; nobody receives it.
        assertGt(vault.sharePrice(), before);
        assertGt(vault.totalValue(), 2000e6 - 1e6);
    }

    function test_the_tank_is_never_the_depositors_money() public {
        // The vault holds the author's tank, and it counts for nothing.
        assertEq(vault.totalValue(), 0);
        assertEq(vault.idle(), 0);
        assertEq(vault.tank().balance, MINIMUM_TANK - REVIEW_FEE);
        assertEq(usd.balanceOf(address(vault)), MINIMUM_TANK - REVIEW_FEE);
    }

    function test_the_vault_reads_its_own_value_from_the_venue() public {
        depositAs(vault, alice, 100_000e6);
        deployCapital(80_000e6, 1e18);

        // Long spot and short the perp: the position is worth the same after
        // a move, which is the point of holding both.
        uint256 before = vault.totalValue();
        setPrice(SPOT, 70_000e6, 0);
        setPrice(PERP, 70_000e6, 0);
        assertEq(vault.totalValue(), before);

        // Funding is the return, and the vault sees it without being told.
        vm.prank(operator);
        venue.accrueFunding(PERP, 100e6);
        assertEq(vault.totalValue(), before + 100e6);
    }

    // Leaving ---------------------------------------------------------------

    function test_a_withdrawal_the_idle_balance_covers_is_paid_at_once() public {
        uint256 shares = depositAs(vault, alice, 1000e6);
        vm.prank(alice);
        (uint256 assets, uint256 claimId) = vault.redeem(shares, alice);
        assertEq(claimId, 0);
        assertEq(usd.balanceOf(alice), assets);
        // She was the only depositor, so the fee she paid on the way in was
        // already hers again; leaving costs her the fee once.
        assertApproxEqAbs(assets, 999e6, 2);
        assertEq(vault.balanceOf(alice), 0);
    }

    function test_the_leaver_pays_the_spread() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        deployCapital(150_000e6, 1e18);
        setPrice(SPOT, PRICE, 100);
        setPrice(PERP, PRICE, 100);

        // Value at mark is what the vault is worth; leaving is worth less,
        // because a leaver's positions are closed at the other side.
        assertLt(vault.exitValue(), vault.totalValue());
        uint256 priceAtMark = vault.sharePrice();
        vm.prank(bob);
        (uint256 assets,) = vault.redeem(bobShares, bob);
        assertLt(assets, bobShares * priceAtMark / 1e18);
    }

    function test_a_withdrawal_the_idle_balance_cannot_cover_books_a_claim() public {
        uint256 shares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deployCapital(190_000e6, 1e18);

        vm.prank(alice);
        (uint256 assets, uint256 claimId) = vault.redeem(shares, alice);
        assertEq(assets, 0);
        assertEq(claimId, 1);

        // The shares are pending, and still outstanding: Alice has not left
        // yet, and still carries her share of what the close costs.
        Claim memory claim = vault.claimAt(claimId);
        assertTrue(claim.state == ClaimState.Pending);
        assertEq(claim.shares, shares);
        assertEq(vault.balanceOf(alice), 0);
        assertEq(vault.balanceOf(address(vault)), shares);
        assertEq(vault.pendingShares(), shares);
        assertEq(usd.balanceOf(alice), 0);
    }

    function test_anyone_can_settle_a_claim_and_the_leaver_is_paid() public {
        uint256 shares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deployCapital(190_000e6, 1e18);
        vm.prank(alice);
        (, uint256 claimId) = vault.redeem(shares, alice);

        uint256 owed = vault.exitValue() * shares / vault.totalSupply();
        // A passer-by sends it, which is the whole point.
        vm.prank(makeAddr("a passer-by"));
        vault.settle(claimId);

        Claim memory claim = vault.claimAt(claimId);
        assertTrue(claim.state == ClaimState.Paid);
        assertEq(usd.balanceOf(alice), claim.settledFor);
        assertApproxEqRel(claim.settledFor, owed, 0.01e18);
        assertEq(vault.pendingShares(), 0);
        assertEq(vault.balanceOf(address(vault)), 0);
        // Half the vault's positions went with her, and Bob's half stayed.
        assertApproxEqRel(uint256(venue.positionOf(address(vault), SPOT).size), 0.5e18, 0.02e18);
    }

    function test_a_claim_waits_when_the_market_is_outside_the_band() public {
        uint256 shares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deployCapital(190_000e6, 1e18);
        vm.prank(alice);
        (, uint256 claimId) = vault.redeem(shares, alice);

        // The oracle says one thing and the book says another: a close inside
        // the band cannot fill, so the claim waits rather than selling badly.
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE / 2, PRICE);
        vault.settle(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Waiting);
        assertEq(usd.balanceOf(alice), 0);

        // When the book comes back, the same call pays.
        setPrice(SPOT, PRICE, 10);
        vault.settle(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Paid);
        assertGt(usd.balanceOf(alice), 0);
    }

    function test_an_unserved_claim_freezes_the_vault() public {
        uint256 shares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        deployCapital(190_000e6, 1e18);
        vm.prank(alice);
        vault.redeem(shares, alice);

        vm.expectRevert();
        vault.freeze();

        skip(7 days);
        vault.freeze();
        assertTrue(vault.state() == VaultState.Frozen);
        // No new deposits, and the trading key is gone, so unwinding is the
        // only thing left that anyone can do.
        assertFalse(venue.isAgent(address(vault), executor));
        give(bob, 1e6);
        vm.startPrank(bob);
        usd.approve(address(vault), 1e6);
        vm.expectRevert();
        vault.deposit(1e6, bob);
        vm.stopPrank();
    }

    // Fees ------------------------------------------------------------------

    function test_the_streaming_fee_is_charged_for_the_time_it_held_money() public {
        depositAs(vault, alice, 100_000e6);
        uint256 total = vault.totalValue();
        skip(365 days);
        vault.crystallise();

        // Half a per cent of the value, a year of it, paid in new shares.
        uint256 platformValue = vault.balanceOf(platform) * vault.sharePrice() / 1e18;
        assertApproxEqRel(platformValue, total * 50 / 10_000, 0.01e18);
        // The pool is not any poorer: the fee is dilution, not a withdrawal.
        assertEq(vault.totalValue(), total);
    }

    function test_the_profit_fee_is_only_ever_taken_on_a_new_high() public {
        depositAs(vault, alice, 100_000e6);
        deployCapital(50_000e6, 1e17);
        // Settle the mark first, so what follows is the strategy's gain and
        // not the lift Alice's own transaction fee gave her shares.
        skip(1 days);
        vault.crystallise();

        // A gain of a thousand dollars: the author keeps a tenth of it.
        skip(1 days);
        vm.prank(operator);
        venue.accrueFunding(PERP, 10_000e6);
        uint256 gain = 1000e6;
        uint256 tankBefore = vault.tank().balance;
        uint256 platformBefore = vault.balanceOf(platform);
        vault.crystallise();
        uint256 authorValue = vault.balanceOf(author) * vault.sharePrice() / 1e18;
        uint256 inTank = vault.tank().balance - tankBefore;
        assertApproxEqRel(authorValue + inTank, gain / 10, 0.02e18);
        // And the platform takes half as much again from the same gain, on
        // top of the day of streaming fee it was owed anyway.
        uint256 platformTook = (vault.balanceOf(platform) - platformBefore) * vault.sharePrice() / 1e18;
        uint256 streamingForADay = uint256(100_000e6) * 50 / 10_000 / 365;
        assertApproxEqRel(platformTook, gain / 20 + streamingForADay, 0.05e18);

        uint256 high = vault.highWaterMark();
        uint256 authorShares = vault.balanceOf(author);

        // Give it back, and take it again: the recovery is free.
        setPrice(SPOT, 50_000e6, 0);
        vault.crystallise();
        assertEq(vault.highWaterMark(), high);
        setPrice(SPOT, PRICE, 0);
        vault.crystallise();
        assertApproxEqRel(vault.balanceOf(author), authorShares, 0.01e18);
    }

    function test_a_late_crystallisation_costs_nothing() public {
        (Vault other,) = openVault("another", 1);
        depositAs(vault, alice, 100_000e6);
        depositAs(other, alice, 100_000e6);

        for (uint256 i = 0; i < 6; i++) {
            skip(30 days);
            other.crystallise();
        }
        vault.crystallise();
        uint256 once = vault.balanceOf(platform);
        // Charged six times or charged once, the same stretch of time costs
        // the same, give or take what compounding does to it.
        assertApproxEqRel(other.balanceOf(platform), once, 0.01e18);
    }

    function test_auto_fill_keeps_the_tank_topped_up_from_the_authors_own_fee() public {
        depositAs(vault, alice, 100_000e6);
        deployCapital(50_000e6, 1e17);
        uint256 before = vault.tank().balance;

        skip(1 days);
        vm.prank(operator);
        venue.accrueFunding(PERP, 10_000e6);
        vault.crystallise();
        assertGt(vault.tank().balance, before);
        // And never past what it is aiming for.
        assertLe(vault.tank().balance, vault.tank().target);
    }

    // The tank --------------------------------------------------------------

    function test_usage_is_charged_to_the_tank_and_capped_at_the_allowances() public {
        uint64 day = uint64(block.timestamp / 1 days);
        uint256 asked = 50e6; // far past the 2 USDC a day this vault declared
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(EXECUTOR_KEY, ethSigned(vault.usageDigest(day, asked)));
        vault.submitUsage(day, asked, abi.encodePacked(r, s, v));

        assertEq(vault.tank().lastCharge, 2e6);
        assertEq(usd.balanceOf(platform), REVIEW_FEE + 2e6);
    }

    function test_a_statement_nobody_blessed_charges_nothing() public {
        uint64 day = uint64(block.timestamp / 1 days);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(0xBAD, ethSigned(vault.usageDigest(day, 1e6)));
        vm.expectRevert();
        vault.submitUsage(day, 1e6, abi.encodePacked(r, s, v));
    }

    function test_an_empty_tank_becomes_a_debt_rather_than_a_stop() public {
        uint64 day = uint64(block.timestamp / 1 days);
        for (uint256 i = 0; i < 25; i++) {
            (uint8 v, bytes32 r, bytes32 s) =
                vm.sign(EXECUTOR_KEY, ethSigned(vault.usageDigest(day + uint64(i), 2e6)));
            vault.submitUsage(day + uint64(i), 2e6, abi.encodePacked(r, s, v));
        }
        Tank memory tank = vault.tank();
        assertEq(tank.balance, 0);
        assertGt(tank.debt, 0);
        // The strategy keeps running: stopping it with positions open would
        // hurt the depositors, not the author.
        assertTrue(vault.state() == VaultState.Open);
    }

    function ethSigned(bytes32 digest) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", digest));
    }

    // Keys and capital ------------------------------------------------------

    function test_the_vault_names_its_two_accounts_and_no_others() public view {
        assertEq(vault.ACCOUNT_IDLE(), IDLE);
        assertEq(vault.ACCOUNT_VENUE(), VENUE);
        assertEq(vault.VENUE_ID(), VENUE_ID);
    }

    function test_capital_only_ever_moves_between_the_vaults_own_accounts() public {
        depositAs(vault, alice, 100_000e6);
        vm.startPrank(executor);
        vm.expectRevert();
        vault.moveCapital(IDLE, "somewhere else", 1e6);
        vm.expectRevert();
        vault.moveCapital(IDLE, VENUE, 200_000e6);
        vm.stopPrank();
    }

    function test_a_key_nobody_blessed_moves_nothing() public {
        depositAs(vault, alice, 100_000e6);
        vm.prank(alice);
        vm.expectRevert();
        vault.moveCapital(IDLE, VENUE, 1e6);
    }

    function test_a_key_can_only_be_approved_while_the_registry_says_so() public {
        address stranger = makeAddr("another executor");
        vm.expectRevert();
        vault.approveKey(VENUE_ID, stranger);
    }

    function test_a_lapsed_key_can_be_revoked_by_anyone_and_a_live_one_cannot() public {
        vm.prank(bob);
        vm.expectRevert();
        vault.revokeKey(VENUE_ID, executor);

        skip(BLESSING_TTL + 1);
        vm.prank(bob);
        vault.revokeKey(VENUE_ID, executor);
        assertFalse(venue.isAgent(address(vault), executor));
    }

    // Kills -----------------------------------------------------------------

    function test_a_kill_brings_everything_home_and_pays_everyone_out() public {
        uint256 shares = depositAs(vault, alice, 100_000e6);
        deployCapital(90_000e6, 1e18);
        setPrice(SPOT, PRICE, 10);
        setPrice(PERP, PRICE, 10);

        vm.prank(bob);
        vm.expectRevert();
        vault.kill();

        vm.prank(admin);
        vault.kill();
        assertTrue(vault.state() == VaultState.Killed);
        assertEq(venue.positionOf(address(vault), SPOT).size, 0);
        assertEq(venue.positionOf(address(vault), PERP).size, 0);
        assertEq(venue.cashOf(address(vault)), 0);
        assertFalse(venue.isAgent(address(vault), executor));

        // Every withdrawal is now paid in one transaction.
        vm.prank(alice);
        (uint256 assets, uint256 claimId) = vault.redeem(shares, alice);
        assertEq(claimId, 0);
        assertApproxEqRel(assets, 100_000e6, 0.01e18);
    }

    // The build -------------------------------------------------------------

    function test_a_vault_takes_no_money_until_its_strategy_is_built() public {
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        Vault pending = Vault(factory.create("unbuilt", 1, keccak256("source"), MINIMUM_TANK, 2e6));
        vm.stopPrank();

        assertTrue(pending.state() == VaultState.Building);
        give(alice, 1e6);
        vm.startPrank(alice);
        usd.approve(address(pending), 1e6);
        vm.expectRevert();
        pending.deposit(1e6, alice);
        vm.stopPrank();
    }

    function test_a_failed_build_closes_the_vault_and_returns_the_tank() public {
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        Vault failing = Vault(factory.create("broken", 1, keccak256("source"), MINIMUM_TANK, 2e6));
        vm.stopPrank();

        vm.prank(alice);
        vm.expectRevert();
        failing.failBuild(keccak256("the two passes disagreed"));

        vm.prank(builder);
        failing.failBuild(keccak256("the two passes disagreed"));
        assertTrue(failing.state() == VaultState.Closed);
        // The build was charged; the rest is the author's again.
        assertEq(usd.balanceOf(author), MINIMUM_TANK - REVIEW_FEE);
    }

    function test_a_vault_cannot_be_created_without_enough_in_its_tank() public {
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        vm.expectRevert();
        factory.create("thin", 1, keccak256("source"), MINIMUM_TANK - 1, 2e6);
        vm.stopPrank();
    }
}
