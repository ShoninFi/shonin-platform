// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Prices} from "../src/Prices.sol";
import {Vault} from "../src/Vault.sol";
import {VaultFactory} from "../src/VaultFactory.sol";
import {Allowances, UnitPrices} from "../src/interfaces/IPrices.sol";
import {VaultState} from "../src/interfaces/IVault.sol";
import {VaultConfig} from "../src/interfaces/IVaultFactory.sol";
import {Fixture} from "./Fixture.sol";

/// Publishing in two steps: pay to submit, which gives the strategy its id;
/// then, once it is reviewed and built, its author activates it, and only then
/// does it have a vault.
contract PublishingTest is Fixture {
    // Submitting ------------------------------------------------------------

    function test_submitting_costs_the_fee_and_assigns_the_next_id() public {
        assertEq(publishing.count(), 0);
        uint256 first = submitAs(author);
        uint256 second = submitAs(alice);
        assertEq(first, 1);
        assertEq(second, 2);
        assertEq(publishing.submissionOf(first).author, author);
        assertEq(publishing.submissionOf(second).author, alice);
        assertEq(usd.balanceOf(platform), PUBLISH_FEE * 2);
        assertEq(publishing.submissionOf(3).author, address(0));
        assertEq(publishing.submissionOf(0).author, address(0));
    }

    function test_nobody_submits_without_paying() public {
        vm.prank(alice);
        vm.expectRevert();
        publishing.submit(keccak256("upload"));
    }

    // Activating ------------------------------------------------------------

    function test_activation_creates_the_vault_and_its_station_from_the_record() public {
        (uint256 id, bytes32 bundle) = ready();
        address predicted = factory.predictVault(id);
        Vault vault = activate(id, allowancesCosting(2e6), MINIMUM_TANK);
        assertEq(address(vault), predicted);
        assertEq(factory.vaultOf(id), address(vault));
        assertEq(vault.strategyId(), id);
        assertEq(vault.bundleHash(), bundle);
        assertEq(vault.author(), author);
        // The dead shares' cent comes out of the tank.
        assertEq(vault.tank().balance, MINIMUM_TANK - DEAD_ASSETS);
        assertEq(vault.cash(), DEAD_ASSETS);
        assertEq(vault.balanceOf(0x000000000000000000000000000000000000dEaD), DEAD_ASSETS * 1e12);
        assertTrue(vault.state() == VaultState.Open);
        assertEq(vault.homeStation(), stations.stationOf(block.chainid, address(vault)));
        // The author signed for the list, like any holder.
        assertEq(vault.statementOf(author), keccak256(abi.encode(vault.restricted())));
        assertEq(vault.restrictedText(), "SANCTIONED,US");
    }

    function test_only_the_author_activates() public {
        bytes32 terms = factory.terms();
        (uint256 id,) = ready();
        give(alice, MINIMUM_TANK);
        vm.startPrank(alice);
        usd.approve(address(factory), MINIMUM_TANK);
        vm.expectRevert(VaultFactory.NotTheAuthor.selector);
        factory.activate(id, allowancesCosting(2e6), 500, MINIMUM_TANK, 0, "", terms);
        vm.stopPrank();
    }

    function test_nothing_activates_before_it_is_built_and_reviewed() public {
        bytes32 terms = factory.terms();
        uint256 id = submitAs(author);
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        vm.expectRevert(VaultFactory.NotReady.selector);
        factory.activate(id, allowancesCosting(2e6), 500, MINIMUM_TANK, 0, "", terms);
        vm.stopPrank();

        // Built, but its review isn't posted yet.
        register(id, author, keccak256("bundle"));
        vm.startPrank(author);
        vm.expectRevert(VaultFactory.NotReady.selector);
        factory.activate(id, allowancesCosting(2e6), 500, MINIMUM_TANK, 0, "", terms);
        vm.stopPrank();
    }

    function test_a_strategy_activates_once() public {
        bytes32 terms = factory.terms();
        (uint256 id,) = ready();
        activate(id, allowancesCosting(2e6), MINIMUM_TANK);
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        vm.expectRevert(VaultFactory.VaultExists.selector);
        factory.activate(id, allowancesCosting(2e6), 500, MINIMUM_TANK, 0, "", terms);
        vm.stopPrank();
    }

    function test_a_vault_cannot_be_activated_without_enough_in_its_tank() public {
        bytes32 terms = factory.terms();
        (uint256 id,) = ready();
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        vm.expectRevert(VaultFactory.TankTooSmall.selector);
        factory.activate(id, allowancesCosting(2e6), 500, MINIMUM_TANK - 1, 0, "", terms);
        vm.stopPrank();
    }

    function test_the_factory_refuses_terms_out_of_range() public {
        VaultConfig memory config = factory.config();
        config.authorBps = 6000;
        config.platformBps = 5000;
        vm.prank(admin);
        vm.expectRevert(VaultFactory.OutOfRange.selector);
        factory.setConfig(config);
    }

    // Prices ----------------------------------------------------------------

    function modest() internal pure returns (Allowances memory a) {
        a.fuelPerSec = 2_000_000;
        a.memoryMib = 256;
        a.requestsPerMin = 120;
        a.bytesPerDay = 50_000_000;
        a.streams = 4;
    }

    /// The same figure the Rust side computes for the modest shape:
    /// 2 cores × $0.15 × 24 + 0.25 GiB × $0.01 × 24 + 0.05 GB × $0.15.
    function test_a_day_at_the_modest_allowances_costs_what_the_app_says() public view {
        assertEq(prices.dailyCost(modest()), 7_267_500);
        assertEq(prices.costOf(modest(), 1000, 1000 + 1 days), 7_267_500);
    }

    function test_inference_and_gas_are_priced() public view {
        Allowances memory a;
        a.inferenceMsPerMin = 2000;
        // Two seconds a minute is 2,880 seconds a day at the processor rate.
        assertApproxEqAbs(prices.dailyCost(a), 120_000, 1);
        a.inferenceMsPerMin = 0;
        a.gasPerDay = 3e6;
        assertEq(prices.dailyCost(a), 3e6);
        assertEq(prices.costOf(a, 0, 12 hours), 1.5e6);
    }

    function test_a_price_change_waits_out_its_notice_and_never_reaches_back() public {
        UnitPrices memory doubled = publishedPrices();
        doubled.vcpuHour *= 2;
        doubled.gibHour *= 2;
        doubled.gbEgress *= 2;
        uint64 start = uint64(block.timestamp);
        vm.prank(admin);
        prices.schedule(doubled);

        assertEq(prices.dailyCost(modest()), 7_267_500);
        skip(PRICE_NOTICE);
        assertEq(prices.dailyCost(modest()), 2 * 7_267_500);

        // A day before the change at the old prices and a day after at the new.
        uint64 change = start + PRICE_NOTICE;
        assertEq(prices.costOf(modest(), change - 1 days, change + 1 days), 3 * 7_267_500);
        assertEq(prices.costOf(modest(), start, start + 1 days), 7_267_500);
    }

    function test_only_the_admin_schedules_prices() public {
        vm.expectRevert(Prices.NotAdmin.selector);
        prices.schedule(publishedPrices());
    }
}
