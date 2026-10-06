// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LighterReport, LighterShares} from "../../src/drivers/lighter/LighterBoard.sol";
import {LighterVault} from "./LighterVault.sol";

/// Money the station's account put into one of Lighter's public pools is
/// counted in the station's value once it is named with `reclaim`. A pool is
/// named by its index written as an address, and only that exact address
/// names it: an address with any bit set above the index's 48 holds nothing,
/// so the station refuses to list it and the pool's shares count once, in
/// the station's value and in what holds a close back.
contract APoolTheStationHoldsSharesInCountsOnceTest is LighterVault {
    uint48 internal constant POOL = 7;
    uint256 internal constant WORTH = 50e6;

    function setUp() public override {
        super.setUp();
        _fundAndTrade();
        _nextBlock();
        LighterReport memory r = _reading();
        r.pools = new LighterShares[](1);
        r.pools[0] = LighterShares({account: _master(), pool: POOL, shares: 100, value: WORTH});
        vm.prank(executor);
        board.post(address(station), r);
    }

    function test_a_pool_named_under_a_second_address_is_not_counted_twice() public {
        (uint256 before,) = station.value();

        station.reclaim(LIGHTER, address(uint160(POOL)));
        (uint256 named,) = station.value();
        assertEq(named, before + WORTH, "the pool's shares count once it is named");

        // The same pool index under another address: the bits above the
        // index differ, so it names nothing and is refused.
        address again = address(uint160(POOL) | (uint160(1) << 48));
        vm.prank(bob);
        (bool ok,) = address(station).call(abi.encodeCall(station.reclaim, (LIGHTER, again)));
        assertFalse(ok, "an address that is not exactly a pool index was listed");
        assertEq(station.heldIn(LIGHTER).length, 1, "the pool is listed once");

        (uint256 after_,) = station.value();
        assertEq(after_, named, "the same pool was counted a second time under another address");
    }
}
