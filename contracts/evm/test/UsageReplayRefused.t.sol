// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "../src/Vault.sol";
import {IVault, Tank} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// Each usage total is signed later than the last, so replaying an old one
/// can't reset the cap carried forward from an earlier set of allowances.
contract UsageReplayRefusedTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
    }

    function test_replaying_the_last_bill_does_not_erase_the_carried_cap() public {
        // Day 1: the executor bills, and is paid for the day at $2 a day.
        skip(1 days);
        uint64 asOf1 = uint64(vm.getBlockTimestamp());
        bytes memory bill1 = usage(vault, 2e6, asOf1);
        vault.keep(2e6, asOf1, bill1);
        uint256 paidSoFar = usd.balanceOf(platform);

        // Ten more days of running at $2 a day, then the author drops the
        // allowances to $1 a day: $20 of cap is carried for those days.
        skip(10 days);
        vm.prank(author);
        vault.setAllowances(allowancesCosting(1e6));
        Tank memory t = vault.tank();
        assertApproxEqAbs(t.capCarried, 20e6, 2, "the ten days were carried");

        // Anyone replays the day-1 bill: nothing is billed, but the carry is
        // wiped. (The key is still blessed, as a running instance's is.)
        blessExecutorKey(executor, address(vault), bundle);
        vm.expectRevert(IVault.Stale.selector);
        vault.keep(2e6, asOf1, bill1);
        t = vault.tank();
        assertEq(usd.balanceOf(platform), paidSoFar, "the replay itself charged nothing");

        // One more day, and the executor bills for everything used so far.
        skip(1 days);
        uint64 asOf2 = uint64(vm.getBlockTimestamp());
        blessExecutorKey(executor, address(vault), bundle);
        vault.keep(100e6, asOf2, usage(vault, 100e6, asOf2));

        // Owed: ten days at $2 plus one at $1. The tank held it.
        assertApproxEqAbs(
            usd.balanceOf(platform) - paidSoFar, 21e6, 3, "the replay wrote ten days of usage off"
        );
    }
}
