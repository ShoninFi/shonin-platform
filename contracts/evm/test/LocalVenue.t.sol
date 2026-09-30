// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Fill, ILocalVenue, OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Fixture} from "./Fixture.sol";

/// The venue's job is to make one sentence true: a trading key can lose money
/// and cannot take it.
contract LocalVenueTest is Fixture {
    address internal master = makeAddr("master");
    address internal agent = makeAddr("agent");

    function setUp() public override {
        super.setUp();
        give(master, 100_000e6);
        vm.startPrank(master);
        usd.approve(address(venue), type(uint256).max);
        venue.deposit(100_000e6);
        venue.approveAgent(agent, uint64(block.timestamp + 180 days));
        vm.stopPrank();
    }

    function buy(
        bytes32 market,
        uint256 size,
        uint256 limit,
        bool reduceOnly
    ) internal pure returns (OrderRequest[] memory orders) {
        orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: market, isBuy: true, size: size, limitPrice: limit, reduceOnly: reduceOnly});
    }

    function sell(
        bytes32 market,
        uint256 size,
        uint256 limit,
        bool reduceOnly
    ) internal pure returns (OrderRequest[] memory orders) {
        orders = new OrderRequest[](1);
        orders[0] = OrderRequest({
            market: market,
            isBuy: false,
            size: size,
            limitPrice: limit,
            reduceOnly: reduceOnly
        });
    }

    function test_an_agent_can_trade_the_account() public {
        vm.prank(agent);
        Fill[] memory fills = venue.place(master, buy(SPOT, 1e18, PRICE * 2, false));
        assertTrue(fills[0].accepted);
        assertEq(venue.positionOf(master, SPOT).size, int256(1e18));
    }

    function test_an_agent_cannot_take_the_money() public {
        // Every way out of the account is master-only, so the agent's calls
        // move its own balance, not the venue's.
        vm.prank(agent);
        vm.expectRevert();
        venue.withdraw(1e6);

        // What the agent *can* do to `withdraw` is call it for itself, which
        // sends it nothing, because it holds nothing here.
        vm.prank(agent);
        vm.expectRevert();
        venue.withdraw(1);
        assertEq(usd.balanceOf(agent), 0);
    }

    function test_an_agent_cannot_approve_another_key() public {
        address stranger = makeAddr("stranger");
        vm.prank(agent);
        venue.approveAgent(stranger, uint64(block.timestamp + 1 days));
        // It approved an agent on the agent's own account, not on the
        // master's, so the stranger still cannot touch the master's.
        assertFalse(venue.isAgent(master, stranger));
        vm.prank(stranger);
        vm.expectRevert();
        venue.place(master, buy(SPOT, 1e18, PRICE * 2, false));
    }

    function test_a_stranger_cannot_trade_the_account() public {
        vm.prank(makeAddr("stranger"));
        vm.expectRevert();
        venue.place(master, buy(SPOT, 1e18, PRICE * 2, false));
    }

    function test_an_expired_agent_stops_trading() public {
        skip(181 days);
        vm.prank(agent);
        vm.expectRevert();
        venue.place(master, buy(SPOT, 1e18, PRICE * 2, false));
    }

    function test_a_revoked_agent_stops_trading() public {
        vm.prank(master);
        venue.revokeAgent(agent);
        vm.prank(agent);
        vm.expectRevert();
        venue.place(master, buy(SPOT, 1e18, PRICE * 2, false));
    }

    // What the venue will and will not fill ---------------------------------

    function test_an_order_outside_its_limit_does_not_fill() public {
        setPrice(SPOT, PRICE, 20);
        // Nobody at that price is the market's answer, not the venue's, so it
        // is an unfilled order rather than a refusal.
        vm.expectEmit(true, true, true, false);
        emit ILocalVenue.Unfilled(master, SPOT, agent);
        vm.prank(agent);
        Fill[] memory fills = venue.place(master, buy(SPOT, 1e18, PRICE, false));
        assertFalse(fills[0].accepted);
        assertEq(fills[0].reason, "no fill at that price");
        assertEq(venue.positionOf(master, SPOT).size, 0);
    }

    function test_a_rejected_order_leaves_its_siblings_alone() public {
        setPrice(SPOT, PRICE, 20);
        OrderRequest[] memory orders = new OrderRequest[](2);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e17, limitPrice: PRICE * 2, reduceOnly: false});
        orders[1] = OrderRequest({market: SPOT, isBuy: true, size: 1e17, limitPrice: 1, reduceOnly: false});

        vm.prank(agent);
        Fill[] memory fills = venue.place(master, orders);
        assertTrue(fills[0].accepted);
        assertFalse(fills[1].accepted);
        assertEq(venue.positionOf(master, SPOT).size, int256(1e17));
    }

    function test_reduce_only_never_flips_a_position() public {
        vm.startPrank(agent);
        venue.place(master, buy(PERP, 1e18, PRICE * 2, false));
        // Ask to sell four times what is open: it sells exactly what is open.
        Fill[] memory fills = venue.place(master, sell(PERP, 4e18, 1, true));
        vm.stopPrank();
        assertTrue(fills[0].accepted);
        assertEq(fills[0].size, 1e18);
        assertEq(venue.positionOf(master, PERP).size, 0);
    }

    function test_reduce_only_refuses_to_open() public {
        vm.prank(agent);
        Fill[] memory fills = venue.place(master, sell(PERP, 1e18, 1, true));
        assertFalse(fills[0].accepted);
        assertEq(fills[0].reason, "nothing to reduce");
    }

    function test_spot_cannot_be_borrowed_against() public {
        vm.startPrank(agent);
        Fill[] memory tooMuch = venue.place(master, buy(SPOT, 2e18, PRICE * 2, false));
        assertFalse(tooMuch[0].accepted);
        assertEq(tooMuch[0].reason, "spot cannot be borrowed against");

        Fill[] memory short = venue.place(master, sell(SPOT, 1e18, 1, false));
        vm.stopPrank();
        assertFalse(short[0].accepted);
    }

    function test_a_perp_may_be_held_on_margin() public {
        vm.prank(agent);
        venue.place(master, buy(PERP, 1e18, PRICE * 2, false));
        assertEq(venue.cashOf(master), int256(40_000e6));
        assertEq(venue.accountValue(master), 100_000e6);
    }

    function test_a_perp_needs_margin_behind_it() public {
        // 100 units at $60,000 is $6m of notional against $100k of equity.
        vm.prank(agent);
        Fill[] memory fills = venue.place(master, buy(PERP, 100e18, PRICE * 2, false));
        assertFalse(fills[0].accepted);
        assertEq(fills[0].reason, "not enough margin");
    }

    function test_margin_has_to_stay_behind_it() public {
        // Sixteen units is $960k of notional: inside ten times $100k.
        vm.prank(agent);
        venue.place(master, buy(PERP, 16e18, PRICE * 2, false));
        assertEq(venue.grossPerpNotional(master), 960_000e6);

        // Taking the cash out from under it is refused.
        vm.prank(master);
        vm.expectRevert();
        venue.withdraw(50_000e6);
    }

    // Prices, value and funding ---------------------------------------------

    function test_a_short_gains_when_the_price_falls() public {
        vm.prank(agent);
        venue.place(master, sell(PERP, 1e18, 1, false));
        setPrice(PERP, 50_000e6, 0);
        assertEq(venue.accountValue(master), 110_000e6);
    }

    function test_leaving_is_valued_at_the_worse_side() public {
        vm.prank(agent);
        venue.place(master, buy(PERP, 1e18, PRICE * 2, false));
        setPrice(PERP, PRICE, 100); // a 1% spread

        // A long is worth the bid to someone leaving, and the mark to someone
        // staying, and the difference is what the leaver pays.
        assertEq(venue.accountValue(master), 100_000e6);
        assertEq(venue.exitValue(master), venue.accountValue(master) - 300e6);
    }

    function test_funding_moves_from_longs_to_shorts() public {
        vm.prank(agent);
        venue.place(master, sell(PERP, 1e18, 1, false));
        uint256 before = venue.accountValue(master);

        // Longs pay $60 a unit; this account is short one unit, so it gains.
        vm.prank(operator);
        venue.accrueFunding(PERP, 60e6);
        assertEq(venue.accountValue(master), before + 60e6);

        // And the gain is real cash once the position next moves.
        vm.prank(agent);
        venue.place(master, buy(PERP, 1e18, PRICE * 2, true));
        assertEq(venue.accountValue(master), before + 60e6);
        assertEq(venue.positionOf(master, PERP).size, 0);
    }

    function test_only_the_operator_moves_the_market() public {
        vm.expectRevert();
        venue.setPrices(PERP, 1, 1, 1, 1);
        vm.expectRevert();
        venue.accrueFunding(PERP, 1);
    }

    function test_the_master_gets_its_money_back() public {
        vm.prank(master);
        venue.withdraw(100_000e6);
        assertEq(usd.balanceOf(master), 100_000e6);
    }
}
