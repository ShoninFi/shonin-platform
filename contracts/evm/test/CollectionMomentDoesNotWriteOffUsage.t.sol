// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "../src/Vault.sol";
import {Fixture} from "./Fixture.sol";

/// Anyone may send `keep` with the executor's latest signed total, so the
/// moment of a collection can be picked by whoever pays the bill. Usage
/// inside the allowances is charged whatever moment it is collected at: a
/// collection right after a burst charges the burst against the cap left by
/// the collections before it, which is kept for up to a day's allowance: a
/// keeper collecting daily loses none of it.
contract CollectionMomentDoesNotWriteOffUsageTest is Fixture {
    Vault internal vault;

    function setUp() public override {
        super.setUp();
        (vault,) = openVault(); // $2 a day of gas allowed
    }

    function test_usage_inside_the_allowances_is_charged_whoever_picks_the_moment() public {
        // A day of running, collected by the keeper's daily round: $0.50.
        skip(1 days);
        uint64 daily = uint64(vm.getBlockTimestamp());
        vault.keep(0.5e6, daily, usage(vault, 0.5e6, daily));
        assertApproxEqAbs(vault.tank().capCarried, 1.5e6, 1, "the day's unused cap was not kept");

        // In the ten minutes after it the strategy spends $1.40 more: $1.90
        // over the day, inside its $2.
        skip(10 minutes);
        uint64 burst = uint64(vm.getBlockTimestamp());
        uint256 before = usd.balanceOf(platform);

        uint256 snapshot = vm.snapshotState();

        // Left to the keeper's next daily round, all of it is charged.
        skip(1 days - 10 minutes);
        uint64 next = uint64(vm.getBlockTimestamp());
        vault.keep(1.9e6, next, usage(vault, 1.9e6, next));
        assertEq(usd.balanceOf(platform) - before, 1.4e6, "the daily round charges the burst");

        vm.revertToState(snapshot);

        // The author collects right after the burst instead.
        vm.prank(author);
        vault.keep(1.9e6, burst, usage(vault, 1.9e6, burst));

        assertEq(usd.balanceOf(platform) - before, 1.4e6, "a collection timed after the burst wrote it off");
    }
}
