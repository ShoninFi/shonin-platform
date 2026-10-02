// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {ClaimState, IVault} from "../../src/interfaces/IVault.sol";

import {ClaimBase} from "../ClaimBase.sol";
import {Vm} from "forge-std/Vm.sol";

/// A leaver pays at most their own shares' part of the profit fee, through
/// every door: a claim paid at its close (in cash) and the pro-rata fallback
/// (as a slice of shares). The bound is measured from the pool directly: the
/// leaver's shares' part of the pool at mark, less what those shares cost at
/// the high-water mark, times the fee rate. And the transaction fee kept in
/// the pool lifts the price above the mark, so a profit fee is owed on it.
contract ExitFeeBoundTest is ClaimBase {
    uint256 internal constant OFFSET = 1e12;
    uint256 internal constant RATE = 1500;

    /// No idle floor, so nearly all the cash can go to the venue.
    function noFloor() internal {
        vm.prank(author);
        vault.setIdleFloor(0);
        skip(1 days);
        blessExecutorKey(executor, address(vault), bundle);
    }

    /// The most `shares` can owe in profit fee right now, at mark.
    function ownFee(uint256 shares) internal view returns (uint256) {
        (uint256 atMark,) = station.value();
        (uint256 units,) = vault.units();
        uint256 pool = vault.cash() + atMark * (WAD - units) / WAD + 1;
        uint256 theirs = pool * shares / (vault.totalSupply() + OFFSET);
        uint256 basis = vault.mark() * shares / 1e30;
        return theirs > basis ? (theirs - basis) * RATE / 10_000 : 0;
    }

    function setUpPool(
        uint256 a,
        uint256 b,
        uint256 gainBps,
        bool payday
    ) internal returns (uint256 bobShares) {
        noFloor();
        depositAs(vault, alice, a);
        bobShares = depositAs(vault, bob, b);
        uint256 cash = vault.cash();
        // Most of the pool in BTC at the venue, a dollar left home.
        deploySpot(cash - 1e6, cash - 1e6, (cash - 1e6) * 9 / 10 * 1e18 / PRICE);
        if (payday) {
            vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
            blessExecutorKey(executor, address(vault), bundle);
            value(vault);
        }
        setPrice(SPOT, PRICE * (10_000 + gainBps) / 10_000, 0);
        value(vault);
    }

    /// The claim's close pays the author and the platform at most the fee
    /// the leaver's own shares owe.
    function testFuzz_a_claims_cash_fee_is_at_most_its_own_share(
        uint256 a,
        uint256 b,
        uint256 gainBps,
        uint256 part,
        bool payday
    ) public {
        a = bound(a, 1000e6, 5_000_000e6);
        b = bound(b, 1000e6, 5_000_000e6);
        gainBps = bound(gainBps, 0, 5000);
        uint256 bobShares = setUpPool(a, b, gainBps, payday);
        uint256 shares = bobShares * bound(part, 1, 100) / 100;
        uint256 id = claimFor(bob, shares);
        uint256 bound_ = ownFee(shares);
        uint256 feeHolders = usd.balanceOf(author) + usd.balanceOf(platform);
        vault.settleClaim(id);
        if (vault.claimAt(id).state != ClaimState.Paid) return;
        uint256 fee = usd.balanceOf(author) + usd.balanceOf(platform) - feeHolders;
        assertLe(fee, bound_ + 1, "a claim's close charged more than its own shares owed");
        // Not asserted: zero fee with no market gain. The transaction fee
        // every deposit leaves in the pool lifts it above the mark, so a
        // profit fee is owed on it.
    }

    uint256 internal fbBound;
    uint256 internal fbNetPerShare;
    uint256 internal fbFeeShares;

    function netPerShareNow() internal view returns (uint256) {
        (uint256 atMark,) = station.value();
        uint256 pool = vault.cash() + atMark + 1;
        uint256 supply = vault.totalSupply() + OFFSET;
        uint256 basis = vault.mark() * supply / 1e30;
        uint256 fee = pool > basis ? (pool - basis) * RATE / 10_000 : 0;
        return (pool - fee) * 1e18 / supply;
    }

    function feeShares() internal view returns (uint256) {
        return vault.balanceOf(author) + vault.balanceOf(platform);
    }

    function fallBack(uint256 rid, bool viaClaim) internal {
        vm.warp(vault.requestAt(rid).earliest + 1 days);
        blessExecutorKey(executor, address(vault), bundle);
        fbFeeShares = feeShares();
        vm.prank(bob);
        if (viaClaim) {
            uint256 id = vault.claimInstead(rid);
            vm.warp(vault.claimAt(id).bookedAt + 1 days + 1);
            fbFeeShares = feeShares();
            vm.prank(bob);
            vault.takeProRataClaim(id);
        } else {
            vault.takeProRata(rid);
        }
    }

    /// The fallback's slice is worth at most the fee the leaver's own shares
    /// owe, at the pool's net price.
    function testFuzz_the_fallbacks_slice_is_at_most_its_own_share(
        uint256 a,
        uint256 b,
        uint256 gainBps,
        uint256 part,
        bool viaClaim
    ) public {
        a = bound(a, 1000e6, 5_000_000e6);
        b = bound(b, 1000e6, 5_000_000e6);
        gainBps = bound(gainBps, 0, 5000);
        uint256 shares = setUpPool(a, b, gainBps, false) * bound(part, 1, 100) / 100;
        // A market outside the band: nothing prices or closes for a day.
        setPrice(SPOT, PRICE * (10_000 + gainBps) / 10_000, 500);
        vm.prank(bob);
        uint256 rid = vault.requestRedeem(shares, bob);
        fbBound = ownFee(shares);
        fbNetPerShare = netPerShareNow();
        fallBack(rid, viaClaim);
        uint256 slice = feeShares() - fbFeeShares;
        assertLe(
            slice * fbNetPerShare / 1e18, fbBound + 1, "the fallback's slice was worth more than its fee"
        );
    }
}

/// The transaction fee a deposit leaves in the pool raises the pool's value
/// per share above the high-water mark, so with no market move at all a
/// payday's profit fee is the fee rate on that gain and no more.
contract TransactionFeeIsProfitTest is ClaimBase {
    function test_payday_mints_profit_fee_on_transaction_fees_alone() public {
        depositAs(vault, alice, 1_000_000e6);
        vm.warp(nextMondayAfter(vault.lastPayment()) + 1);
        vm.recordLogs();
        value(vault);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 minted;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics[0] == IVault.FeesPaid.selector) {
                (uint256 toAuthor, uint256 toPlatform,,) =
                    abi.decode(logs[i].data, (uint256, uint256, uint256, uint256));
                minted = toAuthor + toPlatform;
            }
        }
        // Profit-fee shares only (the holding fee is handed over separately).
        // At about $1 a share: 15% of the $1,000 transaction fee, ~$150.
        emit log_named_decimal_uint("profit fee minted with no market move, $", minted / 1e12, 6);
        assertGt(minted, 0);
        // Worth at most the fee rate on the $1,000 left in the pool, at the
        // price after payday.
        uint256 worth = minted * vault.sharePrice() / 1e18;
        assertLe(worth, 1000e6 * 1500 / 10_000 + 1, "payday took more than the fee rate on the gain");
    }
}
