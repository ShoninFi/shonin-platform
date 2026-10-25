// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Publishing} from "../src/Publishing.sol";
import {Fixture} from "./Fixture.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A fee the dollar won't send on is kept, owed to the platform. A platform
/// of zero (which the dollar never pays) or the contract itself (which
/// would pay itself and keep the money) would keep every fee here for good,
/// so neither is taken.
contract PublishingRefusesAPlatformThatWouldKeepEveryFeeTest is Fixture {
    function test_a_zero_platform_is_refused() public {
        vm.expectRevert(Publishing.BadPlatform.selector);
        new Publishing(IERC20(address(usd)), address(0), PUBLISH_FEE);
    }

    function test_the_contract_itself_as_platform_is_refused() public {
        address next = vm.computeCreateAddress(address(this), vm.getNonce(address(this)));
        vm.expectRevert(Publishing.BadPlatform.selector);
        new Publishing(IERC20(address(usd)), next, PUBLISH_FEE);
    }

    function test_any_other_platform_is_taken() public {
        Publishing fees = new Publishing(IERC20(address(usd)), platform, PUBLISH_FEE);
        assertEq(fees.platform(), platform);
    }
}
