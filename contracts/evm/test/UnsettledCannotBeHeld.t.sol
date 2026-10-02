// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {VaultState} from "../src/interfaces/IVault.sol";
import {ClaimBase} from "./ClaimBase.sol";

/// Anyone may settle, and `settle` never reverts, so a holder who files a
/// one-wei withdrawal every slot and records each valuation can only race the
/// keeper, never hold the stopped unwind off: settling and unwinding in one
/// transaction always goes through, and once the station is found empty no
/// further request is taken. A probe, expected to pass: if it fails, a
/// halted vault can be kept from closing.
contract UnsettledCannotBeHeldTest is ClaimBase {
    Keeper internal keeper;

    function setUp() public override {
        super.setUp();
        keeper = new Keeper();
    }

    function _only(bytes32 name) internal pure returns (bytes32[] memory names) {
        names = new bytes32[](1);
        names[0] = name;
    }

    function _nextSlot() internal {
        uint256 t = vm.getBlockTimestamp();
        vm.warp(t - (t % SLOT) + SLOT);
    }

    function test_a_griefer_filing_each_slot_cannot_keep_a_stopped_station_from_unwinding() public {
        depositAs(vault, alice, 100_000e6);
        uint256 bobShares = depositAs(vault, bob, 100_000e6);
        deploySpot(190_000e6, 190_000e6, 3e18);

        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);
        station.stop();

        // Bob files a one-wei withdrawal and records the valuation that
        // prices it, every slot, so the vault is always "unsettled" when a
        // naive keeper looks. He has to settle the previous slot himself
        // first, since a valuation can't be recorded over an unsettled one.
        for (uint256 i = 0; i < 5; i++) {
            vm.prank(bob);
            vault.requestRedeem(1, bob);
            _nextSlot();
            vm.startPrank(bob);
            vault.settle(type(uint256).max);
            vault.recordValuation();
            vm.stopPrank();
            assertTrue(vault.unsettled(), "the griefer's request was not priced");
            vm.prank(bob);
            vm.expectRevert(Station.NotAllowed.selector);
            station.unwindStopped(_only("local"));
        }

        // The keeper settles and unwinds in one transaction: nothing can get
        // between the two.
        keeper.settleAndUnwind(vault, station, _only("local"));
        assertEq(spotSize(), 0, "the position is still open");
        assertEq(usd.balanceOf(address(station)), 0, "idle dollars stayed at the station");

        // Bob tries again; the empty station is found and no request is taken.
        vm.prank(bob);
        vault.requestRedeem(1, bob);
        _nextSlot();
        vault.recordValuation();
        vault.settle(type(uint256).max);
        _nextSlot();
        vault.recordValuation();
        vm.prank(bob);
        vm.expectRevert();
        vault.requestRedeem(1, bob);
        vault.close();
        assertTrue(vault.state() == VaultState.Closed, "the vault did not close");
        bobShares;
    }
}

contract Keeper {
    function settleAndUnwind(Vault vault, Station station, bytes32[] calldata names) external {
        vault.settle(type(uint256).max);
        station.unwindStopped(names);
    }
}
