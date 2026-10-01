// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {IVault, StationPost, Valuation} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// A station bridges only to a chain the vault trades on, where a station
/// counts what it sends; anywhere else is refused and nothing leaves.
contract BridgeOnlyToTheVaultsChainsTest is TwoChainVault {
    uint256 internal constant THIRD = 1234;

    /// A valuation with the deposit in, so `totalValue()` reports it.
    function funded() internal returns (uint256 valueBefore) {
        depositTwo(alice, 100_000e6);
        valueWith(0, 0);
        valueBefore = vault.totalValue();
        assertApproxEqAbs(valueBefore, 100_000e6, 1e6);
    }

    function test_a_send_to_a_chain_without_a_station_is_refused() public {
        uint256 valueBefore = funded();

        vm.startPrank(executor);
        station.pull(5000e6);
        vm.expectRevert(Station.NoRoute.selector);
        station.bridge("local-bridge", THIRD, 5000e6, "");
        vm.stopPrank();

        // Nothing left the vault's own accounts: the next valuation still
        // sees it all.
        valueWith(0, 0);
        assertApproxEqAbs(
            vault.totalValue(), valueBefore, 1, "money left for a chain where nothing counts it"
        );
    }
}
