// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {Fixture} from "./Fixture.sol";

/// Over random sizes and gains, a deposit followed at once by a withdrawal
/// never returns more than went in, whether the pool sits above or below its
/// mark, and the holder who stayed keeps their price.
contract RoundTripFuzzTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function testFuzz_a_deposit_then_a_withdrawal_never_returns_more_than_it_put_in(
        uint256 first,
        uint256 gain,
        uint256 second,
        bool above
    ) public {
        first = bound(first, MINIMUM_DEPOSIT, 10_000_000e6);
        second = bound(second, MINIMUM_DEPOSIT, 10_000_000e6);
        gain = bound(gain, 0, first / 2);

        depositAs(vault, alice, first);
        vm.prank(executor);
        station.pull(first / 2);
        if (above) {
            give(address(station), gain);
        } else {
            deal(address(usd), address(station), first / 2 - gain);
        }
        value(vault);
        uint256 priceBefore = vault.sharePrice();

        uint256 shares = depositAs(vault, bob, second);
        uint256 paid = redeemAs(vault, bob, shares);
        assertLe(paid, second, "a round trip returned more than it put in");

        value(vault);
        assertGe(vault.sharePrice() + 1, priceBefore, "the holder who stayed paid for the round trip");
    }
}
