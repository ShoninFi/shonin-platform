// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "../src/Vault.sol";
import {Fixture} from "./Fixture.sol";

/// Once a collection has reached a halted vault's grace end the allowances
/// earn no more, but the cap that collection left unused is still carried:
/// a newer total (the gas of the agent's last orders, sent in the grace and
/// landing after) is charged from it. So a collection reaching the grace's
/// end doesn't by itself mean nothing more can be charged; only one that
/// also leaves no cap carried does.
contract ChargedFromTheCarriedCapPastTheGraceTest is Fixture {
    Vault internal vault;

    function setUp() public override {
        super.setUp();
        (vault,) = openVault(); // $2 a day allowed
    }

    function test_a_later_total_is_charged_after_a_collection_reached_the_grace_end() public {
        skip(12 hours); // $1 of cap earned
        vm.prank(admin);
        vault.kill();
        uint64 graceEnd = uint64(vm.getBlockTimestamp()) + HALT_GRACE;

        // The first collection past the grace: $0.40 of what was earned.
        vm.warp(graceEnd + 5);
        vault.keep(0.4e6, graceEnd + 5, usage(vault, 0.4e6, graceEnd + 5));
        assertEq(vault.tank().lastCollection, graceEnd, "the collection did not reach the grace end");
        assertGt(vault.tank().capCarried, 0.5e6, "no cap carried");

        // The agent's last order lands and the executor signs its final total.
        vm.warp(graceEnd + 100);
        uint256 before = usd.balanceOf(platform);
        vault.keep(0.45e6, graceEnd + 100, usage(vault, 0.45e6, graceEnd + 100));
        assertEq(usd.balanceOf(platform) - before, 0.05e6, "the later total was not charged");
    }
}
