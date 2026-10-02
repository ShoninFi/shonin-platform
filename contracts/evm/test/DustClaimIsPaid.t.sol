// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalDriver} from "../src/LocalDriver.sol";
import {LocalVenue} from "../src/LocalVenue.sol";
import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";
import {ClaimState, IVault, VaultState} from "../src/interfaces/IVault.sol";
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
}
