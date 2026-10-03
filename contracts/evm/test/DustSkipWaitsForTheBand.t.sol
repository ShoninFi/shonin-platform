// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {LocalVenue} from "../src/LocalVenue.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A pushed mark at one venue blocks a claim's close rather than making that
/// venue's real account look like dust and leaving the leaver's share of it
/// behind.
contract DustSkipWaitsForTheBandTest is Fixture {
    LocalVenue internal venue2;
    LocalDriver internal driver2;
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        venue2 = new LocalVenue(usd, operator, MARGIN_BPS);
        driver2 = new LocalDriver(venue2, IERC20(address(usd)));
        vm.prank(admin);
        attestation.proposeDriver("local2", address(driver2));
        skip(TIMELOCK);
        vm.prank(operator);
        venue2.listMarket(PERP, true, PRICE);
        give(address(venue), 10_000_000e6);
        give(address(venue2), 10_000_000e6);

        uint256 id = submitAs(author);
        bundle = keccak256(abi.encode("bundle", id));
        ChainNeed[] memory chains = new ChainNeed[](1);
        chains[0].chain = "base";
        chains[0].drivers = new bytes32[](2);
        chains[0].drivers[0] = "local";
        chains[0].drivers[1] = "local2";
        bytes32[] memory none = new bytes32[](0);
        bytes32[] memory restricted = new bytes32[](2);
        restricted[0] = "SANCTIONED";
        restricted[1] = "US";
        bytes32 digest = strategies.registrationDigest(
            id, author, keccak256("source"), bundle, keccak256("manifest"), chains, none, restricted
        );
        strategies.register(
            id,
            author,
            keccak256("source"),
            bundle,
            keccak256("manifest"),
            chains,
            none,
            restricted,
            signed(BUILDER_KEY, digest),
            signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")))
        );
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(bundle, 1));

        (, string memory text) = factory.restrictedFor(id);
        bytes memory statement = statementBy(authorKey, factory.predictVault(id), author, text);
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        vault = Vault(
            factory.activate(id, allowancesCosting(2e6), 0, MINIMUM_TANK, 0, statement, factory.terms())
        );
        vm.stopPrank();
        blessExecutorKey(executor, address(vault), bundle);
        station = stationOf(vault);
    }

    function test_a_leaver_is_paid_without_their_share_of_an_account_whose_mark_is_pushed_outside_the_band()
        public
    {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 150_000e6);

        // The executor deploys everything: 150k into venue 1 as 2.5 BTC
        // spot; 20k into venue 2 as margin behind 3 BTC of perp at 60k
        // (11% margin, inside the venue's 10% rule); 80k idle at the
        // station. The vault's cash is zero.
        vm.startPrank(executor);
        station.pull(vault.cash());
        station.approveKey("local", executor);
        station.approveKey("local2", executor);
        station.deposit("local", 150_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 2.5e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        station.deposit("local2", 20_000e6);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 3e18, limitPrice: PRICE, reduceOnly: false});
        venue2.place(address(station), orders);
        vm.stopPrank();
        assertEq(vault.cash(), 0);
        (uint256 mark2,) = driver2.valueOf(address(station));
        assertEq(mark2, 20_000e6, "venue 2 holds 20k of equity");
        value(vault);
        assertApproxEqAbs(vault.totalValue(), 250_000e6, 1e6);

        // Alice (40%) asks to leave; the cash can't pay, so a claim is
        // booked. Her share at the venues' own prices is 100k.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        uint256 claimId = vault.claimCount();
        assertEq(claimId, 1, "no claim was booked");

        // In the block the claim is settled, venue 2's mark is pushed 11%
        // below its oracle: the perp's equity reads as nothing. Every
        // valuation at this price is void, and the band rule says a close
        // through it must be refused.
        vm.prank(operator);
        venue2.setPrices(PERP, 53_333e6, PRICE, 53_333e6, 53_333e6);
        (mark2,) = driver2.valueOf(address(station));
        assertEq(mark2, 0, "the pushed mark makes the account read as dust");
        assertTrue(station.outsideBand(), "the station is outside the band at this price");

        vault.settleClaim(claimId);

        emit log_named_uint("alice paid (usd 6dp)", usd.balanceOf(alice));
        emit log_named_uint("alice's share at the oracle (usd 6dp)", 100_000e6);

        // What the band rule promises: nothing is closed and the claim
        // waits, as `NobodyPaidOutsideTheBand` shows for one venue.
        assertTrue(
            vault.claimAt(claimId).state == ClaimState.Waiting,
            "a claim was paid through a venue whose mark sits outside the band"
        );
        assertEq(usd.balanceOf(alice), 0, "the leaver was paid without their share of the pushed account");
    }

    /// The same push, measured from the other side: once the mark is back,
    /// the 8k alice was not paid for venue 2 belongs to bob.
    function test_what_the_leaver_was_not_paid_stays_with_the_stayers() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 150_000e6);
        vm.startPrank(executor);
        station.pull(vault.cash());
        station.approveKey("local", executor);
        station.approveKey("local2", executor);
        station.deposit("local", 150_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 2.5e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        station.deposit("local2", 20_000e6);
        orders[0] =
            OrderRequest({market: PERP, isBuy: true, size: 3e18, limitPrice: PRICE, reduceOnly: false});
        venue2.place(address(station), orders);
        vm.stopPrank();
        value(vault);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        uint256 claimId = vault.claimCount();

        vm.prank(operator);
        venue2.setPrices(PERP, 53_333e6, PRICE, 53_333e6, 53_333e6);
        vault.settleClaim(claimId);
        // The next block, the mark is where the oracle is again.
        vm.prank(operator);
        venue2.setPrices(PERP, PRICE, PRICE, PRICE, PRICE);

        (, uint256 exit) = station.value();
        uint256 bobWorth = (vault.cash() + exit) * bobShares / vault.totalSupply();
        emit log_named_uint("bob's worth after the close (usd 6dp)", bobWorth);
        // Bob put in 150k and nothing was earned: the leaver's exit should
        // leave him within fees of that, never 8k richer.
        assertLe(bobWorth, 150_000e6 + 500e6, "the stayer kept the leaver's share of the pushed account");
    }
}
