// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "../src/Station.sol";
import {Vault} from "../src/Vault.sol";
import {IVault} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";

/// The idle floor is measured on the cash, the stations as last valued and
/// what was sent out since, so a run of pulls between two valuations can't
/// step it down.
contract IdleFloorHoldsTest is Fixture {
    Vault internal vault;
    bytes32 internal bundle;
    Station internal station;

    function setUp() public override {
        super.setUp();
        (vault, bundle) = openVault();
        station = stationOf(vault);
    }

    /// The most `toStation` lets through right now, by its own arithmetic:
    /// the station's figure from the last valuation is zero here, so the
    /// "value" it measures the floor against is just the cash left.
    function allowedNow(uint16 floorBps) internal view returns (uint256) {
        uint256 cash = vault.cash();
        uint256 value = cash + 1;
        uint256 floorNow = (value * floorBps + 9999) / 10_000;
        return cash > floorNow ? cash - floorNow : 0;
    }

    function test_the_floor_holds_across_several_pulls_in_one_block() public {
        // Alice is the whole vault: 100k, valued with an empty station. The
        // author set a 5% floor at activation.
        depositAs(vault, alice, 100_000e6);
        // One more valuation, so the reported value includes her money.
        value(vault);
        (uint16 floorBps,,) = vault.idleFloor();
        assertEq(floorBps, 500);
        uint256 total = vault.totalValue();
        assertApproxEqAbs(total, 100_000e6, 1e6);
        uint256 floor = total * floorBps / 10_000;

        // The executor takes everything the floor allows: 95k.
        vm.startPrank(executor);
        station.pull(allowedNow(floorBps));
        assertGe(vault.cash(), floor, "the first pull respected the floor");

        // Then, in the same block, keeps pulling 95% of whatever is left.
        uint256 pulls;
        while (vault.cash() > 1e6 && pulls < 20) {
            uint256 again = allowedNow(floorBps);
            if (again == 0) break;
            (bool ok,) = address(station).call(abi.encodeCall(station.pull, (again)));
            if (!ok) break;
            pulls++;
        }
        vm.stopPrank();

        emit log_named_uint("pulls in one block", pulls);
        emit log_named_uint("cash left (micro-dollars)", vault.cash());
        emit log_named_uint("floor the author set (micro-dollars)", floor);

        // The author promised 5% of the vault idle, with a day's notice before
        // lowering it. The cash must still be there.
        assertGe(vault.cash(), floor, "the idle floor was stepped down to nothing between valuations");
    }
}
