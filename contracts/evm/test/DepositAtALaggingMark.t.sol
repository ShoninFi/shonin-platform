// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Valuation} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// A station is valued at the higher of its mark and its exit side, so a
/// deposit priced where the venue's bid leads its mark pays what the pool
/// could sell for: the holders who stay aren't diluted, and a round trip
/// around the lagging mark returns no more than went in.
contract DepositAtALaggingMarkTest is ClaimBase {
    function test_a_deposit_at_a_bid_above_the_mark_does_not_dilute_the_stayers() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 3e18); // 180k of BTC, 10k of venue cash, 10k of vault cash

        // The market rallies and the venue's bid leads its mark by 2%; the
        // mark will catch up a slot later. The pool is worth 10k + 10k +
        // 3 x 61,200 = 203.6k to anyone who could sell it now.
        vm.prank(operator);
        venue.setPrices(SPOT, PRICE, PRICE, PRICE * 102 / 100, PRICE * 103 / 100);
        assertApproxEqAbs(worth(bobShares), 101_800e6, 100e6);
        // What bob's shares are worth net of the profit fee they owe, which
        // is what a newcomer joins against (the blended mark).
        value(vault);
        uint256 bobRealisable = net(bobShares);

        // Carol's deposit is priced at this valuation: at the mark side.
        uint256 carolShares = depositCarol(100_000e6);
        Valuation memory v = latest();
        assertGt(v.grossExit, v.gross - 1, "the exit side was not above the mark side");
        assertEq(v.grossExit, v.gross, "the clamp did not hold the exit side at the mark");

        // The mark catches up to where the bid already was.
        setPrice(SPOT, PRICE * 102 / 100, 0);
        value(vault);

        emit log_named_uint("bob's realisable worth before carol joined", bobRealisable);
        emit log_named_uint("bob's worth after, at the caught-up mark", net(bobShares));
        emit log_named_uint("carol's worth, having paid 100k", worth(carolShares));

        // Bob and alice should own what they owned, less nothing.
        assertGe(net(bobShares) + 1e6, bobRealisable, "a deposit at a lagging mark diluted the stayer");
        assertGe(net(aliceShares) + 1e6, bobRealisable, "a deposit at a lagging mark diluted the stayer");

        // And carol's round trip must not return more than she put in.
        uint256 paid = redeemAs(vault, carol, carolShares);
        emit log_named_uint("carol's round trip returned", paid);
        assertLe(paid, 100_000e6, "a round trip around a lagging mark paid more than went in");
    }

    /// What `shares` own now, less their part of the profit fee owed at the
    /// latest valuation.
    function net(uint256 shares) internal view returns (uint256) {
        (, uint256 exit) = station.value();
        uint256 total = vault.cash() + exit;
        uint256 supply = vault.totalSupply() + 1e12;
        uint256 basis = vault.mark() * supply / 1e30;
        uint256 fee = total > basis ? (total - basis) * 1500 / 10_000 : 0;
        return (total - fee) * shares / supply;
    }
}
