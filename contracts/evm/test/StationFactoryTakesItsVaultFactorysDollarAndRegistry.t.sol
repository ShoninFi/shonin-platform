// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {StationFactory} from "../src/StationFactory.sol";
import {Test} from "forge-std/Test.sol";

/// What a vault factory says it is wired to.
contract VaultFactoryWiring {
    address public asset;
    address public attestation;
    address public stationFactory;

    constructor(address asset_, address attestation_, address stationFactory_) {
        asset = asset_;
        attestation = attestation_;
        stationFactory = stationFactory_;
    }
}

/// The station factory is initialised once, for good, and every station it
/// creates values and sends the dollar it was given and pins drivers from
/// the registry it was given. So it takes only the vault factory's own
/// dollar and registry, and only a vault factory that creates its stations
/// here.
contract StationFactoryTakesItsVaultFactorysDollarAndRegistryTest is Test {
    StationFactory internal factory;
    address internal implementation;
    address internal usdc = makeAddr("usdc");
    address internal registry = makeAddr("registry");

    function setUp() public {
        factory = new StationFactory(address(this));
        implementation = address(new Station());
    }

    function test_a_dollar_that_is_not_the_vault_factorys_is_refused() public {
        VaultFactoryWiring vaults = new VaultFactoryWiring(usdc, registry, address(factory));
        vm.expectRevert(StationFactory.NotTheVaultFactorys.selector);
        factory.initialize(registry, makeAddr("another dollar"), address(vaults), implementation);
    }

    function test_a_registry_that_is_not_the_vault_factorys_is_refused() public {
        VaultFactoryWiring vaults = new VaultFactoryWiring(usdc, registry, address(factory));
        vm.expectRevert(StationFactory.NotTheVaultFactorys.selector);
        factory.initialize(makeAddr("another registry"), usdc, address(vaults), implementation);
    }

    function test_a_vault_factory_that_creates_its_stations_elsewhere_is_refused() public {
        VaultFactoryWiring vaults =
            new VaultFactoryWiring(usdc, registry, makeAddr("another station factory"));
        vm.expectRevert(StationFactory.NotTheVaultFactorys.selector);
        factory.initialize(registry, usdc, address(vaults), implementation);
    }

    function test_an_address_that_is_no_vault_factory_is_refused() public {
        vm.expectRevert();
        factory.initialize(registry, usdc, makeAddr("nothing"), implementation);
    }

    function test_the_vault_factorys_own_dollar_and_registry_are_taken() public {
        VaultFactoryWiring vaults = new VaultFactoryWiring(usdc, registry, address(factory));
        factory.initialize(registry, usdc, address(vaults), implementation);
        assertEq(factory.asset(), usdc);
        assertEq(factory.attestation(), registry);
        assertEq(factory.vaultFactory(), address(vaults));
    }
}
