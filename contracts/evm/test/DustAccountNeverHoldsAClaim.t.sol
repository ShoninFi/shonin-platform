// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {LocalVenue} from "../src/LocalVenue.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {ClaimState, IVault, VaultState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A venue account worth no more than a few dollar-units is left where it is
/// in a fractional close, so its rounding can't hold every leaver's close: a
/// large claim at a healthy venue is paid on its first try.
contract DustAccountNeverHoldsAClaimTest is Fixture {
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
        venue2.listMarket(SPOT, false, PRICE);
        give(address(venue), 10_000_000e6);

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

    function test_three_millionths_of_a_dollar_at_one_venue_never_hold_up_a_claim() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 150_000e6);

        // Everything goes to the station: $100,000 into the first venue as
        // a spot position, three dollar-units into the second as a dust
        // position, the rest idle at the station. The vault's cash is zero.
        vm.startPrank(executor);
        station.pull(vault.cash());
        station.approveKey("local", executor);
        station.approveKey("local2", executor);
        station.deposit("local", 100_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        // 5e7 of size at $60,000: notional 3 units, so the account's cash is
        // spent and its only holding is the position.
        station.deposit("local2", 3);
        orders[0] = OrderRequest({market: SPOT, isBuy: true, size: 5e7, limitPrice: PRICE, reduceOnly: false});
        venue2.place(address(station), orders);
        vm.stopPrank();
        assertEq(vault.cash(), 0);
        // The second venue has a 1% spread: 3 at the mark, 2 at the bid.
        vm.prank(operator);
        venue2.setPrices(SPOT, PRICE, PRICE, PRICE * 99 / 100, PRICE * 101 / 100);
        (uint256 mark2, uint256 exit2) = driver2.valueOf(address(station));
        assertEq(mark2, 3, "the dust account is not 3 at the mark");
        assertEq(exit2, 2, "the dust account is not 2 at the bid");
        value(vault);

        // Alice (40% of the vault) asks to leave; the cash can't pay, so a
        // claim is booked.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        uint256 claimId = vault.claimCount();
        assertEq(claimId, 1, "no claim was booked");

        // Every market is inside the band, the first venue holds $100,000
        // in good order, and the claim's fraction of it is worth $40,000.
        assertFalse(station.outsideBand());
        vault.settleClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Paid, "dust at one venue held the claim");
        assertGt(usd.balanceOf(alice), 39_000e6, "alice was not paid her fraction");
    }
}
