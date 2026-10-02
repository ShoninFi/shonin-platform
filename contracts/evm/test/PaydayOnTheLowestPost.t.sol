// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {StationPost} from "../src/interfaces/IVault.sol";
import {TwoChainVault} from "./TwoChainVault.sol";

/// When two blessed keys' posts for a station disagree, deposits pay the
/// highest figure, but the profit fee and payday are measured on the lowest
/// mark: a gain only the higher post shows is never charged as profit.
contract PaydayOnTheLowestPostTest is TwoChainVault {
    function nextMonday(uint256 t) internal pure returns (uint256) {
        uint256 sinceMonday = t / 1 days + 3;
        return (sinceMonday - (sinceMonday % 7) + 4) * 1 days;
    }

    /// Blessings last 72 hours; a week on, the machines have re-attested.
    function rebless() internal {
        blessExecutorKey(executor, address(vault), bundle);
        bytes memory journal = journalFor(UNWINDER_IMAGE, unwinder, leafFor(address(vault), bundle));
        attestation.bless(sealOf(journal), journal, block.chainid, address(vault), bundle, new bytes32[](0));
    }

    function test_payday_charges_no_profit_only_the_higher_post_shows() public {
        depositTwo(alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(90_000e6);
        station.bridge("local-bridge", OTHER, 90_000e6, "");
        vm.stopPrank();
        // A payday with no gain, so the next one is a week out.
        vm.warp(nextMonday(vault.lastPayment()));
        rebless();
        valueWith(90_000e6, 90_000e6);
        uint256 authorBefore = vault.balanceOf(author);

        // The next payday: the executor's post shows a 10k gain, the
        // unwinder's (read a block later, say) a 20k gain.
        vm.warp(nextMonday(vault.lastPayment()));
        rebless();
        uint64 time = open();
        post(time, postOf(100_000e6, 100_000e6), EXECUTOR_KEY);
        post(time, postOf(110_000e6, 110_000e6), UNWINDER_KEY);
        closeAfterDeadline(time);

        // The author's 10% of the gain both posts agree on is 1k, and no
        // more is minted.
        uint256 minted = vault.balanceOf(author) - authorBefore;
        uint256 lowerGross = vault.cash() + 100_000e6 + 1;
        uint256 worth = minted * lowerGross / (vault.totalSupply() + 1e12);
        assertLe(worth, 1010e6, "payday charged a fee on a gain only the higher post showed");
    }
}
