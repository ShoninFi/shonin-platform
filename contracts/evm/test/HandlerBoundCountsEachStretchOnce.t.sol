// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Vault} from "../src/Vault.sol";
import {Tank} from "../src/interfaces/IVault.sol";
import {Fixture} from "./Fixture.sol";
import {Deps, VaultHandler} from "./invariant/VaultHandler.sol";

/// The handler, with a collection at a chosen signed time and a look at the
/// per-collection bound `_checkCharge` would hold the next collection to.
contract BoundHandler is VaultHandler {
    constructor(Deps memory deps) VaultHandler(deps) {}

    function collectAt(uint64 asOf, uint256 more) external {
        _begin();
        Tank memory t = vault.tank();
        uint256 total = t.usageSettled + more;
        bytes memory signature = _usage(total, asOf);
        billAsOf = asOf;
        vm.prank(keeper);
        vault.keep(total, asOf, signature);
        _end(true, "collectAt");
    }

    function setAllowancesNow(uint256 perDay, uint256 fuel) external {
        _setAllowances(perDay, fuel);
    }

    /// What `_checkCharge` would allow a collection signed at `at`, beyond
    /// the last collection's carry: the allowances' cost since the last
    /// collection.
    function sinceBound(uint64 at) external view returns (uint256) {
        uint64 start = lastCollection;
        uint64 end = _capEnd(at);
        if (end < start) end = start;
        uint64 changed = _capEnd(allowanceSince[allowanceSince.length - 1]);
        return _capBetween(start, changed > end ? changed : end);
    }
}

/// The invariant handler's per-collection bound counts each stretch of time
/// once, as the vault does.
///
/// A change of allowances closes the cap's interval at the old ones and
/// carries it. A total signed before the change and collected after it is
/// charged against that carry, and the vault never counts the time up to
/// the change again: the next collection's cap is its carry plus the new
/// allowances from the change. So the handler's next bound starts at the
/// change too. A bound that started at the older signed time would price
/// the stretch up to the change a second time, and a vault that charged it
/// twice would pass.
contract HandlerBoundCountsEachStretchOnceTest is Fixture {
    Vault internal vault;
    BoundHandler internal handler;

    function setUp() public override {
        super.setUp();
        (vault,) = openVault();
        handler = new BoundHandler(
            Deps({
                vault: vault,
                usd: usd,
                venue: venue,
                attestation: attestation,
                prices: prices,
                factory: factory,
                driver: driver,
                admin: admin,
                platform: platform,
                operator: operator,
                author: author,
                authorKey: authorKey,
                executorKey: EXECUTOR_KEY,
                attestorKey: ATTESTOR_KEY,
                executorImage: EXECUTOR_IMAGE,
                project: PROJECT
            })
        );
        handler.bootstrap();
    }

    function test_theBoundAfterAnOlderTotalCountsOnlyWhatTheVaultHasNotCounted() public {
        // Times kept here: under via-IR a read of `block.timestamp` after a
        // warp can come back as the one before it.
        uint64 t0 = uint64(vm.getBlockTimestamp()) + 1 hours;
        vm.warp(t0);
        handler.collectAt(t0, 0);

        uint64 tA = t0 + 6 hours;
        uint64 t1 = tA + 6 hours;
        vm.warp(t1);
        handler.setAllowancesNow(5e6, 1_000_000);

        // The total signed at tA, collected only now, after the change.
        vm.warp(t1 + 1 hours);
        handler.collectAt(tA, 0);
        assertEq(vault.tank().lastCollection, tA);
        assertEq(handler.tankBreaks(), 0, handler.lastBreak());

        // What the vault can still add to its carry for a collection at t3:
        // the allowances in force since they were set, and no more.
        uint64 t3 = t1 + 2 hours;
        vm.warp(t3);
        uint256 uncounted = prices.costOf(vault.allowances(), t1, t3);
        assertEq(
            handler.sinceBound(t3),
            uncounted,
            "the handler's bound after an older total is not the allowances' cost since the change"
        );

        // A collection then, billing far more than its cap, still passes
        // every check: the bound allows all the vault may charge.
        handler.collectAt(t3, 1000e6);
        assertEq(handler.tankBreaks(), 0, handler.lastBreak());
    }
}
