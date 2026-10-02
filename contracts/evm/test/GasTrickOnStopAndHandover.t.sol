// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {OrderRequest} from "../src/interfaces/ILocalVenue.sol";
import {Fixture} from "./Fixture.sol";

/// No gas limit a caller picks lets a stop, or a lapsed key's replacement,
/// go through with the key still approved at the venue but forgotten by the
/// station.
contract GasTrickOnStopAndHandoverTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
        give(address(venue), 1_000_000e6);
    }

    function deployed() internal {
        depositAs(vault, alice, 100_000e6);
        vm.startPrank(executor);
        station.pull(90_000e6);
        station.approveKey("local", executor);
        station.deposit("local", 90_000e6);
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: true, size: 1e18, limitPrice: PRICE, reduceOnly: false});
        venue.place(address(station), orders);
        vm.stopPrank();
        assertTrue(venue.agentValidUntil(address(station), executor) != 0);
    }

    function test_no_gas_limit_lets_a_stop_through_with_the_key_still_approved_at_the_venue() public {
        deployed();
        vm.prank(admin);
        vault.kill();
        skip(HALT_GRACE);

        uint256 passes;
        uint256 kept;
        uint256 lowestPass;
        for (uint256 gas = 20_000; gas <= 600_000; gas += 500) {
            uint256 snap = vm.snapshotState();
            vm.prank(bob);
            (bool ok,) = address(station).call{gas: gas}(abi.encodeWithSignature("stop()"));
            if (ok) {
                if (lowestPass == 0) lowestPass = gas;
                passes++;
                if (
                    station.approvedKey("local") != address(0)
                        || venue.agentValidUntil(address(station), executor) != 0
                ) kept++;
            }
            vm.revertToState(snap);
        }
        emit log_named_uint("lowest gas at which stop() went through", lowestPass);
        assertGt(passes, 0, "stop() never went through at any gas limit in the sweep");
        assertEq(kept, 0, "a gas limit let stop() through with the key still approved at the venue");
    }

    function test_no_gas_limit_lets_a_handover_through_with_the_old_key_still_approved() public {
        deployed();
        // The running key's blessing lapses; a fresh instance is blessed.
        skip(BLESSING_TTL + 1);
        address fresh = vm.addr(0xF4E5);
        blessExecutorKey(fresh, address(vault), bundle);
        assertFalse(attestation.isBlessedFor(executor, block.chainid, address(vault), bundle));

        uint256 passes;
        uint256 kept;
        for (uint256 gas = 30_000; gas <= 600_000; gas += 500) {
            uint256 snap = vm.snapshotState();
            vm.prank(bob);
            (bool ok,) = address(station).call{gas: gas}(abi.encodeCall(Station.approveKey, ("local", fresh)));
            if (ok) {
                passes++;
                if (venue.agentValidUntil(address(station), executor) != 0) kept++;
            }
            vm.revertToState(snap);
        }
        assertGt(passes, 0, "the handover never went through at any gas limit in the sweep");
        assertEq(kept, 0, "a gas limit let the handover through with the old key still approved");
    }
}
