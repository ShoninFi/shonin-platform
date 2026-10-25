// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Publishing} from "../src/Publishing.sol";
import {Fixture} from "./Fixture.sol";
import {ERC20} from "openzeppelin/token/ERC20/ERC20.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A dollar whose issuer can blocklist an address: every transfer to or
/// from it reverts.
contract BlocklistingDollar is ERC20 {
    mapping(address => bool) public blocklisted;

    constructor() ERC20("Blocklisting dollar", "USD") {}

    function decimals() public pure override returns (uint8) {
        return 6;
    }

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function setBlocklisted(address who, bool on) external {
        blocklisted[who] = on;
    }

    function _update(address from, address to, uint256 value) internal override {
        require(!blocklisted[from] && !blocklisted[to], "blocklisted");
        super._update(from, to, value);
    }
}

/// The publishing fee's receiver is fixed. If the dollar's issuer
/// blocklists it, authors can still submit: each fee is kept, owed to the
/// platform, and sent on once the dollar lets it through.
contract PublishingFeeIsOwedNeverLostTest is Fixture {
    BlocklistingDollar internal dollar;
    Publishing internal fees;

    function setUp() public override {
        super.setUp();
        dollar = new BlocklistingDollar();
        fees = new Publishing(IERC20(address(dollar)), platform, PUBLISH_FEE);
    }

    function _submit(address who) internal returns (uint256 id) {
        dollar.mint(who, PUBLISH_FEE);
        vm.startPrank(who);
        dollar.approve(address(fees), PUBLISH_FEE);
        id = fees.submit(keccak256(abi.encode("upload", who)));
        vm.stopPrank();
    }

    function test_the_fee_reaches_the_platform_at_once() public {
        _submit(alice);
        assertEq(dollar.balanceOf(platform), PUBLISH_FEE);
        assertEq(fees.owed(), 0);
    }

    function test_a_blocklisted_receiver_stops_no_submission_and_every_fee_stays_owed() public {
        dollar.setBlocklisted(platform, true);
        assertEq(_submit(alice), 1);
        assertEq(_submit(bob), 2);
        assertEq(fees.owed(), 2 * PUBLISH_FEE, "a fee was lost");
        assertEq(dollar.balanceOf(address(fees)), 2 * PUBLISH_FEE);
        vm.expectRevert(bytes("blocklisted"));
        fees.collect();
        assertEq(fees.owed(), 2 * PUBLISH_FEE);
    }

    function test_what_is_owed_goes_on_once_the_receiver_is_unblocked() public {
        dollar.setBlocklisted(platform, true);
        _submit(alice);
        dollar.setBlocklisted(platform, false);
        vm.prank(bob);
        assertEq(fees.collect(), PUBLISH_FEE);
        assertEq(dollar.balanceOf(platform), PUBLISH_FEE);
        assertEq(fees.owed(), 0);
    }

    function test_the_next_fee_carries_what_is_owed_with_it() public {
        dollar.setBlocklisted(platform, true);
        _submit(alice);
        dollar.setBlocklisted(platform, false);
        _submit(bob);
        assertEq(dollar.balanceOf(platform), 2 * PUBLISH_FEE);
        assertEq(fees.owed(), 0);
    }
}
