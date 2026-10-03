// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {LocalVenue} from "../src/LocalVenue.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Call} from "../src/interfaces/IDriver.sol";

import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {ClaimState, IVault, Valuation, VaultState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A claim worth fewer dollar-units at the stations than they have parts
/// brings nothing home however often it is closed. It is dust: it is paid its
/// cash share and done, so it can't hold a vault open or freeze it.
contract DustClaimIsPaidTest is Fixture {
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

        // A strategy trading through two venues on this chain.
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

        // Activated with an idle floor of zero.
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

    function test_a_claim_worth_less_than_a_unit_per_part_is_paid_as_dust_not_left_to_freeze_the_vault()
        public
    {
        depositAs(vault, alice, 90_000e6);

        // The executor deploys everything: a third at each venue as cash, a
        // third idle at the station. The vault's cash is exactly zero.
        vm.startPrank(executor);
        station.pull(vault.cash());
        station.deposit("local", 30_000e6);
        station.deposit("local2", 30_000e6);
        vm.stopPrank();
        assertEq(vault.cash(), 0);
        value(vault);

        // Alice asks to redeem 2.9 trillionths of a share: about $0.0000029.
        // Priced, that is a payout of one unit after the transaction fee,
        // which the empty cash can't pay, so it is booked as a claim.
        vm.prank(alice);
        vault.requestRedeem(2.9e12, alice);
        value(vault);
        uint256 claimId = vault.claimCount();
        assertEq(claimId, 1, "no claim was booked");
        assertTrue(vault.claimAt(claimId).state == ClaimState.Pending);

        // Serving it closes a fraction that rounds to nothing at each of the
        // three parts, so nothing comes home and it is "not a try".
        vault.settleClaim(claimId);
        assertTrue(
            vault.claimAt(claimId).state == ClaimState.Paid,
            "a dust claim is left waiting for ever instead of being settled as dust"
        );

        // A week later nobody can freeze the vault with it.
        skip(7 days);
        vm.prank(bob);
        vm.expectRevert(IVault.NotAllowed.selector);
        vault.freeze(claimId);
        assertTrue(vault.state() != VaultState.Frozen, "a few millionths of a dollar froze the vault");
    }

    /// The same dust claim, while the second venue refuses every close: a
    /// close that failed proves nothing, so the claim is not judged dust. It
    /// waits with its shares intact, not counted as a try, and is paid as
    /// dust once a close goes through.
    function test_a_dust_claim_whose_close_fails_waits_and_is_paid_as_dust_once_a_close_goes_through()
        public
    {
        depositAs(vault, alice, 90_000e6);
        vm.startPrank(executor);
        station.pull(vault.cash());
        station.deposit("local", 30_000e6);
        station.deposit("local2", 30_000e6);
        vm.stopPrank();
        assertEq(vault.cash(), 0);
        value(vault);
        vm.prank(alice);
        vault.requestRedeem(2.9e12, alice);
        value(vault);
        uint256 claimId = vault.claimCount();
        assertEq(claimId, 1, "no claim was booked");
        uint256 shares = vault.claimAt(claimId).shares;

        // The second venue starts refusing closes; it still answers reads.
        (uint256 worth,) = station.value();
        bytes memory honest = address(driver2).code;
        vm.etch(
            address(driver2),
            address(new CloseRefusingDriver(new LocalDriver(venue2, IERC20(address(usd))))).code
        );
        (uint256 atMark,) = station.value();
        assertEq(atMark, worth, "the stations no longer read their worth");
        vm.prank(address(vault));
        vm.expectRevert(CloseRefusingDriver.Refused.selector);
        station.unwind(1);

        vault.settleClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Waiting, "a failed close was judged dust");
        assertEq(vault.claimAt(claimId).shares, shares, "the claim's shares changed");
        assertEq(vault.claimAt(claimId).lastAttemptAt, 0, "a failed close counted as a try");
        assertEq(vault.claimAt(claimId).paid, 0);

        // The venue answers again: the close goes through, and it is dust.
        vm.etch(address(driver2), honest);
        vault.settleClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Paid, "the dust claim was not settled");
    }

    /// A claim the last valuation counted above dust, whose fraction of the
    /// stations now reads below a unit at exit prices after a crash: a close
    /// would sell a sliver of the position and bring nothing home. It is not
    /// dust, so the close is refused outright and nothing it sold stands.
    function test_a_claim_counted_above_dust_whose_close_would_bring_nothing_home_is_refused_and_sells_nothing(
    ) public {
        depositAs(vault, alice, 90_000e6);
        // Everything at the first venue, almost all of it in 1.49 BTC spot.
        vm.startPrank(executor);
        station.pull(vault.cash());
        station.approveKey("local", executor);
        station.deposit("local", 90_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1.49e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        assertEq(vault.cash(), 0);
        value(vault);

        // Alice asks out for 1.5e-10 of the vault; the empty cash can't pay
        // it, so it is booked as a claim, counted at about 13.5 units of the
        // stations: above the dust of nine.
        uint256 shares = vault.totalSupply() * 15 / 1e11;
        vm.prank(alice);
        vault.requestRedeem(shares, alice);
        value(vault);
        uint256 claimId = vault.claimCount();
        assertEq(claimId, 1, "no claim was booked");
        Valuation memory last = vault.valuationAt(vault.valuationCount() - 1);
        uint256 fraction = shares * 1e18 / vault.totalSupply();
        assertGt((last.gross - 1 - last.cash) * fraction / 1e18, 9, "the claim was counted as dust");

        // BTC crashes to a twentieth: the claim's part now reads under a
        // unit at exit prices.
        setPrice(SPOT, PRICE / 20, 0);
        (, uint256 exitNow) = station.value();
        assertLt(exitNow * fraction / 1e18, 1, "the claim's part still reads a unit");
        int256 size = venue.positionOf(address(station), SPOT).size;
        int256 venueCash = venue.cashOf(address(station));

        vm.expectRevert(IVault.NotYet.selector);
        vault.settleClaim(claimId);
        assertTrue(vault.claimAt(claimId).state == ClaimState.Pending, "the claim moved on");
        assertEq(venue.positionOf(address(station), SPOT).size, size, "a close that paid nobody sold");
        assertEq(venue.cashOf(address(station)), venueCash, "a close that paid nobody moved the venue's cash");
    }
}

/// A venue's driver that reads as the honest one but refuses every close.
contract CloseRefusingDriver {
    error Refused();

    LocalDriver internal immutable honest;

    constructor(LocalDriver honest_) {
        honest = honest_;
    }

    function close(address, uint256, uint16) external pure returns (Call[] memory, bool) {
        revert Refused();
    }

    fallback(bytes calldata data) external returns (bytes memory) {
        (bool ok, bytes memory out) = address(honest).staticcall(data);
        if (!ok) {
            assembly {
                revert(add(out, 32), mload(out))
            }
        }
        return out;
    }
}
