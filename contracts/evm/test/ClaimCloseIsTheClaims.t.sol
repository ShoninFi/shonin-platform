// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {ClaimState} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// While pro-rata units are outstanding, a claim's close is measured on the
/// shares' part alone, so everything it brings home is the claim's: none of
/// it goes to the unit holders, whose fraction of what is left grows to
/// match. The claim is paid its fraction, at the next valuation or through
/// the fallback, and nothing is closed twice.
contract ClaimCloseIsTheClaimsTest is Fixture {
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

    function test_a_queued_claim_taken_through_the_fallback_gets_its_fraction_while_units_exist() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        uint256 carolShares = depositCarol(100_000e6);
        // 285k idle at the station, 15k of cash (the 5% floor).
        vm.prank(executor);
        station.pull(285_000e6);

        // Carol leaves through the fallback: 5k of cash and a third of the
        // station as units.
        vm.prank(carol);
        uint256 carolRequest = vault.requestRedeem(carolShares, carol);
        skip(24 hours + SLOT);
        vm.prank(carol);
        vault.takeProRata(carolRequest);
        (uint256 units,) = vault.units();
        assertApproxEqRel(units, uint256(1e18) / 3, 0.001e18, "carol holds a third of the station");

        // Valuations resume. Alice's withdrawal can't be paid from 10k: a claim.
        value(vault);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);
        assertTrue(vault.claimAt(1).state == ClaimState.Pending);

        // What alice's shares are fairly worth: half of what the shares own,
        // which is the cash plus two thirds of the station.
        (, uint256 stationExit) = station.value();
        uint256 sharesOwn = vault.cash() + stationExit * (1e18 - units) / 1e18;
        uint256 fair = sharesOwn * aliceShares / vault.totalSupply();
        assertApproxEqAbs(fair, 100_000e6, 100e6);

        // Serving her closes her third of the station (95k), and all of it
        // lands in cash.
        uint256 cashBefore = vault.cash();
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);
        uint256 home = vault.cash() - cashBefore;
        assertApproxEqAbs(home, 95_000e6, 100e6, "the units took a cut of the claim's close");

        // Nothing is priced for a day; alice takes the fallback for her
        // queued claim. She should get her fraction.
        skip(24 hours + SLOT);
        vm.prank(alice);
        vault.takeProRataClaim(1);
        uint256 paid = usd.balanceOf(alice);
        assertGe(
            paid + 100e6, fair, "a queued claim taken through the fallback was paid less than its fraction"
        );
    }

    function test_a_claim_served_while_units_exist_is_paid_at_the_next_valuation() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        uint256 carolShares = depositCarol(100_000e6);
        vm.prank(executor);
        station.pull(285_000e6);

        vm.prank(carol);
        uint256 carolRequest = vault.requestRedeem(carolShares, carol);
        skip(24 hours + SLOT);
        vm.prank(carol);
        vault.takeProRata(carolRequest);

        value(vault);
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);

        // Her fraction is closed and comes home; the next valuation pays the
        // claim, as it does when no units are outstanding.
        uint256 stationBefore = usd.balanceOf(address(station));
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);
        uint256 closed = stationBefore - usd.balanceOf(address(station));
        assertApproxEqAbs(closed, 95_000e6, 100e6, "her third of the station was closed");

        value(vault);
        assertTrue(
            vault.claimAt(1).state == ClaimState.Paid,
            "the claim whose fraction was closed could not be paid, because the units took a cut of it"
        );
    }
}
