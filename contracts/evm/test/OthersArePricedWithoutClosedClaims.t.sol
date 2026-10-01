// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {ClaimState, Valuation} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// A closed claim's shares own a fixed amount, set aside, and are paid
/// exactly that; every other request priced at the same valuation is priced
/// without them, so a move at the stations after the close is the stayers'
/// alone and nobody pays for anyone else's move.
contract OthersArePricedWithoutClosedClaimsTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;
    address internal carol;
    uint256 internal carolKey;

    function setUp() public override {
        super.setUp();
        (carol, carolKey) = makeAddrAndKey("carol");
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    function depositCarol(uint256 amount) internal returns (uint256 shares) {
        bytes memory statement = statementBy(carolKey, address(vault), carol, vault.restrictedText());
        give(carol, amount);
        vm.startPrank(carol);
        usd.approve(address(vault), amount);
        vault.requestDeposit(amount, carol, statement);
        vm.stopPrank();
        value(vault);
        shares = vault.balanceOf(carol);
    }

    /// Pull `pulled` to the station and buy `btc` BTC of spot with it, so
    /// the station's worth follows the spot price one for one.
    function deployLong(uint256 pulled, uint256 btc) internal {
        vm.startPrank(executor);
        station.pull(pulled);
        station.approveKey("local", executor);
        station.deposit("local", pulled);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] = OrderRequest({market: SPOT, isBuy: true, size: btc, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        setPrice(SPOT, PRICE, 0);
    }

    function latest() internal view returns (Valuation memory) {
        return vault.valuationAt(vault.valuationCount() - 1);
    }

    /// Three equal holders. 210k is deployed (180k of spot, 30k idle at the
    /// venue); 90k stays as cash. Alice's withdrawal becomes a claim, is
    /// closed (her third of the station comes home and is set aside for
    /// her), and then the station halves in value before the next slot.
    /// Bob's ordinary redemption, priced in that slot, is paid the pool's
    /// average price, which includes Alice's fixed cash. He is overpaid and
    /// Carol, who stays, pays for it.
    function test_a_redeem_priced_beside_a_closed_claim_is_paid_its_fraction() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        uint256 carolShares = depositCarol(100_000e6);
        deployLong(210_000e6, 3e18);

        // Alice leaves; 90k of cash can't pay ~100k: a claim.
        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        assertEq(vault.claimCount(), 1);
        assertTrue(vault.claimAt(1).state == ClaimState.Pending);

        // Her third is closed: 1 BTC sold, 70k comes home, set aside for her
        // with her 30k share of the cash. Her shares now own exactly that.
        uint256 cashBeforeClose = vault.cash();
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);
        uint256 home = vault.cash() - cashBeforeClose;
        assertApproxEqAbs(home, 70_000e6, 100e6, "her third came home");

        // Before the next slot, spot halves: the station (2 BTC + 20k) is
        // worth 80k instead of 140k. Alice's money is already in cash.
        setPrice(SPOT, PRICE / 2, 0);
        (, uint256 stationExit) = station.value();
        assertApproxEqAbs(stationExit, 80_000e6, 100e6);

        // Bob asks to redeem 70% of his shares in the same slot that prices
        // Alice's claim. The cash left after Alice is paid (~60k) covers it.
        uint256 bobRedeem = bobShares * 7 / 10;
        vm.prank(bob);
        vault.requestRedeem(bobRedeem, bob);

        uint256 cashBefore = vault.cash();
        value(vault);
        Valuation memory v = latest();
        assertTrue(vault.claimAt(1).state == ClaimState.Paid, "alice's claim was priced first");
        uint256 alicePaid = vault.claimAt(1).paid;
        uint256 bobPaid = usd.balanceOf(bob);
        assertGt(bobPaid, 0, "bob's redemption was paid from cash, not booked as a claim");

        // What Bob's shares were worth once Alice had taken what she owned:
        // his fraction of the cash that was left and the station as it stood.
        // (Alice's fee slice is at most a few shares; her shares are treated
        // as burned.)
        uint256 poolAfterAlice = cashBefore - alicePaid + stationExit;
        // The valuation's supply already leaves her closed shares out.
        uint256 supplyAfterAlice = v.supply;
        uint256 bobFair = poolAfterAlice * bobRedeem / supplyAfterAlice;
        uint256 carolBefore = poolAfterAlice * carolShares / supplyAfterAlice;
        uint256 carolAfter = (vault.cash() + stationExit) * carolShares / (vault.totalSupply() + 1e12);

        emit log_named_uint("valuation gross", v.gross);
        emit log_named_uint("valuation supply", v.supply);
        emit log_named_uint("alice owned and was paid", alicePaid);
        emit log_named_uint("bob's shares were worth", bobFair);
        emit log_named_uint("bob was paid", bobPaid);
        emit log_named_uint("carol before bob's redemption", carolBefore);
        emit log_named_uint("carol after bob's redemption", carolAfter);

        // Bob gets at most his fraction (less the 0.1% transaction fee).
        assertLe(bobPaid, bobFair, "a redeem priced beside a closed claim was paid above its fraction");
        // And Carol, who did nothing, should be no worse off than before
        // Bob's move, beyond rounding.
        assertGe(carolAfter + 10e6, carolBefore, "a stayer paid for a leaver's move");
    }

    /// The same mispricing from the other side: a deposit priced beside the
    /// closed claim buys in at the average price, which is above what the
    /// stayers' shares are worth, so the newcomer overpays and the stayers
    /// gain. Either way, whoever moves in that slot is priced wrong.
    function test_a_deposit_priced_beside_a_closed_claim_buys_at_the_stayers_price() public {
        uint256 aliceShares = depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 100_000e6);
        uint256 carolShares = depositCarol(100_000e6);
        deployLong(210_000e6, 3e18);

        vm.prank(alice);
        vault.requestRedeem(aliceShares, alice);
        value(vault);
        vault.settleClaim(1);
        assertTrue(vault.claimAt(1).state == ClaimState.Queued);

        setPrice(SPOT, PRICE / 2, 0);
        (, uint256 stationExit) = station.value();
        (uint256 stationMark,) = station.value();

        // Dave deposits 50k, priced in the slot that pays Alice.
        (address dave, uint256 daveKey) = makeAddrAndKey("dave");
        bytes memory statement = statementBy(daveKey, address(vault), dave, vault.restrictedText());
        give(dave, 50_000e6);
        vm.startPrank(dave);
        usd.approve(address(vault), 50_000e6);
        vault.requestDeposit(50_000e6, dave, statement);
        vm.stopPrank();

        uint256 cashBefore = vault.cash();
        value(vault);
        Valuation memory v = latest();
        uint256 alicePaid = vault.claimAt(1).paid;
        uint256 daveShares = vault.balanceOf(dave);
        assertGt(daveShares, 0);

        // The pool Dave joined, once Alice had taken hers: the cash left plus
        // the station at mark, over the shares that stayed.
        uint256 poolAfterAlice = cashBefore - alicePaid + stationMark;
        // The valuation's supply already leaves her closed shares out.
        uint256 supplyAfterAlice = v.supply;
        // Shares 50k (less the 0.1% fee) should buy at that price.
        uint256 fairShares = (50_000e6 - 50e6) * supplyAfterAlice / poolAfterAlice;
        // Carol's worth before and after Dave joined.
        uint256 carolBefore = (poolAfterAlice - stationMark + stationExit) * carolShares / supplyAfterAlice;
        uint256 carolAfter = (vault.cash() + stationExit) * carolShares / (vault.totalSupply() + 1e12);

        emit log_named_uint("shares dave should have bought", fairShares);
        emit log_named_uint("shares dave bought", daveShares);
        emit log_named_uint("carol before dave joined", carolBefore);
        emit log_named_uint("carol after dave joined", carolAfter);

        assertGe(
            daveShares + daveShares / 1000,
            fairShares,
            "a deposit priced beside a closed claim bought fewer shares than its money was worth"
        );
    }
}
