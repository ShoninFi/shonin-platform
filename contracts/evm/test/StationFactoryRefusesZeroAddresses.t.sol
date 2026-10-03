// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {StationFactory} from "../src/StationFactory.sol";
import {Test} from "forge-std/Test.sol";

/// A station factory initialises once, at an address nobody can deploy to
/// again, so it refuses every zero address it is given.
contract StationFactoryRefusesZeroAddressesTest is Test {
    function test_a_station_factory_refuses_a_zero_vault_factory() public {
        StationFactory factory = new StationFactory(address(this));
        address implementation = address(new Station());
        vm.expectRevert(StationFactory.NotInitialized.selector);
        factory.initialize(makeAddr("registry"), makeAddr("dollar"), address(0), implementation);
    }
}
