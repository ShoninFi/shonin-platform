// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// The home station sends nothing to another chain while a valuation waits
/// for posts, as the vault sends it nothing then: money counted at home at
/// the open can't land elsewhere in time to be counted there too.
contract NoHomeSendWhileValuingTest is TwoChainVault {
    function test_the_home_station_refuses_to_bridge_while_a_valuation_is_open() public {
        depositTwo(alice, 100_000e6);
        vm.prank(executor);
        station.pull(10_000e6);
        uint64 time = open();
        vm.prank(executor);
        vm.expectRevert(Station.NotAllowed.selector);
        station.bridge("local-bridge", OTHER, 10_000e6, "");

        // Once the valuation is closed, it may.
        post(time, postOf(0, 0), EXECUTOR_KEY);
        closeAfterDeadline(time);
        vm.prank(executor);
        station.bridge("local-bridge", OTHER, 10_000e6, "");
    }
}
