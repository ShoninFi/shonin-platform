// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

// Every external or public function that can change state, called with
// random arguments by random callers. Each call is either refused, for an
// unauthorised caller with exactly that refusal and otherwise only for a
// reason the function gives, or it goes through and leaves the books
// consistent: the vault still holds what it owes, no dollar reaches anyone
// the call doesn't pay, and no counter goes backwards.
//
// Coverage: each function and the test that fuzzes it.
//
// Vault (through Valuations, Posts, Claims and Upkeep), in this file:
//   initialize               VaultExternalFunctionsTest.testFuzz_initialize
//   requestDeposit           VaultExternalFunctionsTest.testFuzz_requestDeposit
//   requestRedeem            VaultExternalFunctionsTest.testFuzz_requestRedeem
//   recordValuation          VaultExternalFunctionsTest.testFuzz_recordValuation,
//                            VaultAcrossChainsExternalFunctionsTest.testFuzz_recordValuation
//   postStation              VaultExternalFunctionsTest.testFuzz_postStation,
//                            VaultAcrossChainsExternalFunctionsTest.testFuzz_postStation
//   closeValuation           VaultExternalFunctionsTest.testFuzz_closeValuation,
//                            VaultAcrossChainsExternalFunctionsTest.testFuzz_closeValuation
//   settle                   VaultExternalFunctionsTest.testFuzz_settle
//   settleClaim              VaultExternalFunctionsTest.testFuzz_settleClaim
//   freeze                   VaultExternalFunctionsTest.testFuzz_freeze
//   freezeUnits              VaultExternalFunctionsTest.testFuzz_freezeUnits
//   takeProRata              VaultExternalFunctionsTest.testFuzz_takeProRata
//   claimInstead             VaultExternalFunctionsTest.testFuzz_claimInstead
//   settleClosed             VaultExternalFunctionsTest.testFuzz_settleClosed
//   takeProRataClaim         VaultExternalFunctionsTest.testFuzz_takeProRataClaim
//   reportExitCost           VaultExternalFunctionsTest.testFuzz_reportExitCost,
//                            VaultAcrossChainsExternalFunctionsTest.testFuzz_reportExitCost
//   collectUnits             VaultExternalFunctionsTest.testFuzz_collectUnits
//   serveProRata             VaultExternalFunctionsTest.testFuzz_serveProRata
//   redeemClosed             VaultExternalFunctionsTest.testFuzz_redeemClosed
//   claimClosed              VaultExternalFunctionsTest.testFuzz_claimClosed
//   collectHeld              VaultExternalFunctionsTest.testFuzz_collectHeld
//   toStation                VaultExternalFunctionsTest.testFuzz_toStation
//   fromStation              VaultExternalFunctionsTest.testFuzz_fromStation
//   fundTank                 VaultExternalFunctionsTest.testFuzz_fundTank
//   keep                     VaultExternalFunctionsTest.testFuzz_keep
//   withdrawTank             VaultExternalFunctionsTest.testFuzz_withdrawTank
//   requestTankWithdrawal    VaultExternalFunctionsTest.testFuzz_requestTankWithdrawal
//   withdrawTankUnsigned     VaultExternalFunctionsTest.testFuzz_withdrawTankUnsigned
//   topUpFromShares          VaultExternalFunctionsTest.testFuzz_topUpFromShares
//   setTankKeep              VaultExternalFunctionsTest.testFuzz_setTankKeep
//   setAllowances            VaultExternalFunctionsTest.testFuzz_setAllowances
//   recordStatement          VaultExternalFunctionsTest.testFuzz_recordStatement
//   refreshRestricted        VaultExternalFunctionsTest.testFuzz_refreshRestricted
//   setIdleFloor             VaultExternalFunctionsTest.testFuzz_setIdleFloor
//   retire                   VaultExternalFunctionsTest.testFuzz_retire
//   kill                     VaultExternalFunctionsTest.testFuzz_kill
//   windDownSealed           VaultExternalFunctionsTest.testFuzz_windDownSealed
//   confirmStations          VaultExternalFunctionsTest.testFuzz_confirmStations,
//                            VaultAcrossChainsExternalFunctionsTest.testFuzz_confirmStations_and_cancel
//   cancel                   VaultExternalFunctionsTest.testFuzz_cancel,
//                            VaultAcrossChainsExternalFunctionsTest.testFuzz_confirmStations_and_cancel
//   evict                    VaultExternalFunctionsTest.testFuzz_evict
//   close                    VaultExternalFunctionsTest.testFuzz_close
//   transfer, transferFrom,
//   approve                  VaultExternalFunctionsTest.testFuzz_shares_never_move_between_holders
//
// VaultFactory, StationFactory, Station, AttestationRegistry,
// StrategyRegistry, ReviewRegistry, Publishing, Prices, AuthorProfiles,
// LocalBridge and LocalVenue, in ExternalFunctionsPlatform.t.sol:
//   VaultFactory.activate                 VaultFactoryExternalFunctionsTest.testFuzz_activate
//   VaultFactory.setConfig                VaultFactoryExternalFunctionsTest.testFuzz_setConfig
//   StationFactory.initialize             StationExternalFunctionsTest.testFuzz_factory_initialize
//   StationFactory.createHome             StationExternalFunctionsTest.testFuzz_factory_createHome
//   StationFactory.create                 RemoteStationExternalFunctionsTest.testFuzz_factory_create
//   Station.initialize                    StationExternalFunctionsTest.testFuzz_initialize
//   Station.approveKey                    StationExternalFunctionsTest.testFuzz_approveKey
//   Station.revokeKey                     StationExternalFunctionsTest.testFuzz_revokeKey
//   Station.pull                          StationExternalFunctionsTest.testFuzz_pull
//   Station.push                          StationExternalFunctionsTest.testFuzz_push
//   Station.deposit                       StationExternalFunctionsTest.testFuzz_deposit
//   Station.withdraw                      StationExternalFunctionsTest.testFuzz_withdraw
//   Station.bridge                        StationExternalFunctionsTest.testFuzz_bridge (one chain),
//                                         VaultAcrossChainsExternalFunctionsTest.testFuzz_station_bridge
//   Station.stop()                        StationExternalFunctionsTest.testFuzz_stop
//   Station.stop(bytes32,uint64,bytes)    RemoteStationExternalFunctionsTest.testFuzz_carried_stop,
//                                         StationExternalFunctionsTest.testFuzz_stop (refused at home)
//   Station.unwind                        StationExternalFunctionsTest.testFuzz_unwind
//   Station.closeAll                      StationExternalFunctionsTest.testFuzz_closeAll
//   Station.unwindStopped                 StationExternalFunctionsTest.testFuzz_unwindStopped
//   AttestationRegistry.setTimelock       RegistryExternalFunctionsTest.testFuzz_setTimelock
//   AttestationRegistry.proposeImage,
//     revokeImage, proposeProject,
//     revokeProject, proposeVerifier,
//     retireVerifier, proposeDriver,
//     withdrawDriverProposal,
//     revokeDriver, proposeBridge,
//     withdrawBridgeProposal,
//     revokeBridge                        RegistryExternalFunctionsTest.testFuzz_admin_only
//   AttestationRegistry.bless             RegistryExternalFunctionsTest.testFuzz_bless
//   StrategyRegistry.register             RegistryExternalFunctionsTest.testFuzz_register
//   ReviewRegistry.submitReview           RegistryExternalFunctionsTest.testFuzz_submitReview
//   ReviewRegistry.submitRefusal          RegistryExternalFunctionsTest.testFuzz_submitRefusal
//   Publishing.submit                     RegistryExternalFunctionsTest.testFuzz_submit
//   Prices.schedule                       RegistryExternalFunctionsTest.testFuzz_schedule
//   AuthorProfiles.setProfile             RegistryExternalFunctionsTest.testFuzz_setProfile
//   LocalBridge.carry                     LocalStandInsExternalFunctionsTest.testFuzz_bridge_carry
//   LocalBridge.deliver                   LocalStandInsExternalFunctionsTest.testFuzz_bridge_deliver
//   LocalVenue.listMarket, setPrices,
//     post, accrueFunding, setFunding     LocalStandInsExternalFunctionsTest.testFuzz_venue_operator_only
//   LocalVenue.approveAgent,
//     revokeAgent, deposit, withdraw,
//     place                               LocalStandInsExternalFunctionsTest.testFuzz_venue_accounts
//
// Not fuzzed here:
//   LocalDriver             every function is a view: it only prepares the
//                           calls a station checks and makes, and those are
//                           fuzzed through the station.
//   DevAttestationVerifier  views only; it is exercised through `bless`.
//   LocalUsd.mint           anyone may mint the development dollar by design;
//                           it stands in for the real dollar and is no part of
//                           the platform.

import {Station} from "../../src/Station.sol";
import {Vault} from "../../src/Vault.sol";
import {ImageKind} from "../../src/interfaces/IAttestationRegistry.sol";
import {Allowances} from "../../src/interfaces/IPrices.sol";
import {
    Claim,
    ClaimState,
    IVault,
    Request,
    RequestKind,
    StationPost,
    Tank,
    Transit,
    Valuation,
    ValuationStatus,
    VaultInit,
    VaultState
} from "../../src/interfaces/IVault.sol";
import {ClaimBase} from "../ClaimBase.sol";

import {TwoChainVault} from "../TwoChainVault.sol";
import {Calls} from "./Calls.sol";
import {IERC20Errors} from "openzeppelin/interfaces/draft-IERC6093.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// A vault's books before and after a call.
struct World {
    address[] who;
    uint256[] usdOf;
    uint256 usdSupply;
    uint256 requests;
    uint256 valuations;
    uint256 claims;
    uint256 usageSettled;
    uint64 usageAsOf;
    uint256 mark;
    uint64 lastPayment;
    VaultState state;
    uint256 dead;
    bool[] done;
}

/// The single-chain vault, put into one of several states before each call:
/// at rest, with requests waiting, with a priced valuation unsettled, with a
/// claim booked, retired, killed, closed, or with the fallback's units
/// outstanding.
contract VaultExternalFunctionsTest is ClaimBase, Calls {
    address internal constant DEAD = 0x000000000000000000000000000000000000dEaD;

    uint256 internal claimId;
    uint256 internal lateRequest;

    function setUp() public override {
        super.setUp();
        depositAs(vault, alice, 100_000e6);
        depositAs(vault, bob, 20_000e6);
        deploySpot(60_000e6, 60_000e6, 0.5e18);
        value(vault);
    }

    // The states ------------------------------------------------------------

    uint8 internal constant AT_REST = 0;
    uint8 internal constant WAITING = 1;
    uint8 internal constant PRICED = 2;
    uint8 internal constant CLAIM = 3;
    uint8 internal constant RETIRED = 4;
    uint8 internal constant KILLED = 5;
    uint8 internal constant CLOSED = 6;
    uint8 internal constant UNITS = 7;

    function nextBoundary() internal view returns (uint256) {
        uint256 t = vm.getBlockTimestamp();
        return t - (t % SLOT) + SLOT;
    }

    function requestCarol(uint256 amount) internal {
        bytes memory statement = statementBy(carolKey, address(vault), carol, vault.restrictedText());
        give(carol, amount);
        vm.startPrank(carol);
        usd.approve(address(vault), amount);
        vault.requestDeposit(amount, carol, statement);
        vm.stopPrank();
    }

    /// Bob's withdrawal, with the cash held at the station, becomes a claim.
    function bookClaim() internal {
        uint256 keepHome = vault.cash() - 7000e6;
        vm.prank(executor);
        station.pull(keepHome);
        claimId = claimFor(bob, vault.balanceOf(bob));
    }

    function arrange(uint8 stage) internal returns (uint8) {
        stage %= 8;
        if (stage == WAITING || stage == PRICED) {
            requestCarol(5000e6);
            uint256 shares = vault.balanceOf(alice) / 10;
            vm.prank(alice);
            vault.requestRedeem(shares, alice);
            if (stage == PRICED) {
                vm.warp(nextBoundary());
                vault.recordValuation();
            }
        } else if (stage == CLAIM) {
            bookClaim();
        } else if (stage == RETIRED) {
            vm.prank(author);
            vault.retire();
        } else if (stage == KILLED) {
            vm.prank(admin);
            vault.kill();
            skip(HALT_GRACE);
        } else if (stage == CLOSED) {
            // A claim booked before the kill, and a withdrawal asked for
            // after the last valuation, both left for the closed vault.
            bookClaim();
            vm.prank(admin);
            vault.kill();
            skip(HALT_GRACE);
            station.stop();
            station.unwind(1e18);
            vm.warp(nextBoundary() - 2);
            uint256 shares = vault.balanceOf(alice) / 10;
            vm.prank(alice);
            lateRequest = vault.requestRedeem(shares, alice);
            value(vault);
            vault.close();
            assertTrue(vault.state() == VaultState.Closed);
        } else if (stage == UNITS) {
            // Nothing is priced for a day: alice takes the fallback for a
            // tenth of her shares, and some money comes home since.
            setPrice(SPOT, PRICE, 500);
            uint256 shares = vault.balanceOf(alice) / 10;
            vm.prank(alice);
            uint256 id = vault.requestRedeem(shares, alice);
            vm.warp(vault.requestAt(id).earliest + 1 days);
            vm.prank(alice);
            vault.takeProRata(id);
            setPrice(SPOT, PRICE, 0);
            vm.startPrank(executor);
            station.withdraw("local", 1000e6);
            station.push(1000e6);
            vm.stopPrank();
        }
        return stage;
    }

    /// Alice, bob, carol, the author, the admin, the executor, the platform,
    /// the home station, or anyone at all.
    function pickCaller(uint8 pick, address random) internal view returns (address) {
        uint8 p = pick % 9;
        if (p == 0) return alice;
        if (p == 1) return bob;
        if (p == 2) return carol;
        if (p == 3) return author;
        if (p == 4) return admin;
        if (p == 5) return executor;
        if (p == 6) return platform;
        if (p == 7) return address(station);
        // The vault never calls itself.
        return random == address(vault) ? address(uint160(random) + 1) : random;
    }

    // The books -------------------------------------------------------------

    function look(address caller, address extra) internal view returns (World memory w) {
        w.who = new address[](14);
        w.who[0] = address(vault);
        w.who[1] = address(station);
        w.who[2] = address(venue);
        w.who[3] = address(bridge);
        w.who[4] = alice;
        w.who[5] = bob;
        w.who[6] = carol;
        w.who[7] = author;
        w.who[8] = platform;
        w.who[9] = admin;
        w.who[10] = executor;
        w.who[11] = operator;
        w.who[12] = caller;
        w.who[13] = extra;
        w.usdOf = new uint256[](14);
        for (uint256 i = 0; i < 14; i++) {
            w.usdOf[i] = usd.balanceOf(w.who[i]);
        }
        w.usdSupply = usd.totalSupply();
        w.requests = vault.requestCount();
        w.valuations = vault.valuationCount();
        w.claims = vault.claimCount();
        Tank memory t = vault.tank();
        w.usageSettled = t.usageSettled;
        w.usageAsOf = t.usageAsOf;
        w.mark = vault.mark();
        w.lastPayment = vault.lastPayment();
        w.state = vault.state();
        w.dead = vault.balanceOf(DEAD);
        w.done = new bool[](w.requests);
        for (uint256 i = 0; i < w.requests; i++) {
            w.done[i] = vault.requestAt(i + 1).done;
        }
    }

    function legal(VaultState from, VaultState to) internal pure returns (bool) {
        if (from == to) return true;
        if (from == VaultState.Activating) {
            return to == VaultState.Open || to == VaultState.Cancelled || to == VaultState.Killed;
        }
        if (from == VaultState.Open) {
            return to == VaultState.WindingDown || to == VaultState.Frozen || to == VaultState.Killed;
        }
        if (from == VaultState.WindingDown) {
            return to == VaultState.Frozen || to == VaultState.Killed || to == VaultState.Closed;
        }
        if (from == VaultState.Frozen) return to == VaultState.Killed || to == VaultState.Closed;
        if (from == VaultState.Killed) return to == VaultState.Closed;
        return false;
    }

    /// What the vault holds covers what it owes; no dollar left the vault,
    /// its station, the venue or the bridge for anyone but `payees`, and
    /// none went anywhere untracked; no counter, request or the mark went
    /// backwards; the state moved only forward.
    function consistent(World memory b, address[] memory payees) internal view {
        World memory a = look(b.who[12], b.who[13]);
        Tank memory t = vault.tank();
        (, uint256 reserve) = vault.units();
        uint256 held;
        for (uint256 i = 4; i < 14; i++) {
            if (!seenBefore(a.who, i, a.who[i])) held += vault.heldFor(a.who[i]);
        }
        assertGe(
            usd.balanceOf(address(vault)),
            vault.cash() + t.balance + vault.escrow() + reserve + held,
            "the vault holds less than it owes"
        );

        assertEq(a.usdSupply, b.usdSupply, "dollars were made or destroyed");
        uint256 before;
        uint256 afterwards;
        for (uint256 i = 0; i < 14; i++) {
            if (seenBefore(a.who, i, a.who[i])) continue;
            before += b.usdOf[i];
            afterwards += a.usdOf[i];
            bool system = i < 4;
            if (!system && a.usdOf[i] > b.usdOf[i]) {
                assertTrue(contains(payees, a.who[i]), "money reached someone the call doesn't pay");
            }
        }
        assertEq(afterwards, before, "money went somewhere nobody tracks");

        assertGe(a.requests, b.requests, "a request disappeared");
        assertGe(a.valuations, b.valuations, "a valuation disappeared");
        assertGe(a.claims, b.claims, "a claim disappeared");
        assertGe(a.usageSettled, b.usageSettled, "the usage collected went backwards");
        assertGe(a.usageAsOf, b.usageAsOf, "the usage's moment went backwards");
        assertGe(a.mark, b.mark, "the mark fell");
        assertGe(a.lastPayment, b.lastPayment, "the last payday went backwards");
        assertTrue(legal(b.state, a.state), "the state moved backwards");
        // Only the close moves the dead shares: their part goes to the author.
        if (a.state == VaultState.Closed && b.state != VaultState.Closed) assertEq(a.dead, 0);
        else assertEq(a.dead, b.dead, "the dead shares moved");
        for (uint256 i = 0; i < b.done.length; i++) {
            if (b.done[i]) assertTrue(a.done[i], "a settled request was unsettled");
        }
        assertGe(vault.totalSupply(), a.dead, "fewer shares than the dead ones");
    }

    function isClosed(World memory b) internal pure returns (bool) {
        return b.state == VaultState.Closed;
    }

    // Setting up ------------------------------------------------------------

    function testFuzz_initialize(
        uint8 stage,
        uint8 pick,
        address random,
        uint256 tank,
        uint256 dead
    ) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        VaultInit memory init;
        init.author = caller;
        init.asset = address(usd);
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) =
            callAs(caller, address(vault), abi.encodeCall(IVault.initialize, (init, tank, dead)));
        expectOutcome(ok, err, IVault.NotAllowed.selector, "initialize, again");
        (ok, err) = callAs(
            caller, factory.vaultImplementation(), abi.encodeCall(IVault.initialize, (init, tank, dead))
        );
        expectOutcome(ok, err, IVault.NotAllowed.selector, "initialize, the implementation");
        consistent(b, nobody());
    }

    // Depositors ------------------------------------------------------------

    function testFuzz_requestDeposit(
        uint8 stage,
        uint8 pick,
        address random,
        uint256 assets,
        uint8 to,
        address other,
        bool signs,
        bool funded
    ) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        address receiver = pickCaller(to, other);
        assets = bound(assets, 0, 1_000_000e6);
        bool keyed = receiver == alice || receiver == bob || receiver == author || receiver == carol;
        bytes memory statement;
        if (signs) {
            statement = keyed
                ? statementBy(
                    receiver == carol ? carolKey : keyOf(receiver),
                    address(vault),
                    receiver,
                    vault.restrictedText()
                )
                : abi.encodePacked(keccak256(abi.encode(other)), keccak256(abi.encode(random)), uint8(27));
        }
        if (funded && caller != address(0)) {
            give(caller, assets);
            vm.prank(caller);
            usd.approve(address(vault), assets);
        }
        World memory b = look(caller, receiver);
        uint256 escrow = vault.escrow();
        bool signedNow = signs && keyed;
        bytes4 want;
        if (signs && !keyed) {
            want = IVault.BadSignature.selector;
        } else if (b.state != VaultState.Open) {
            want = IVault.WrongState.selector;
        } else if (assets < MINIMUM_DEPOSIT || receiver == address(0)) {
            want = IVault.TooSmall.selector;
        } else if (!signedNow && vault.statementOf(receiver) != keccak256(abi.encode(vault.restricted()))) {
            want = IVault.NoStatement.selector;
        } else if (usd.allowance(caller, address(vault)) < assets) {
            want = IERC20Errors.ERC20InsufficientAllowance.selector;
        } else if (usd.balanceOf(caller) < assets) {
            want = IERC20Errors.ERC20InsufficientBalance.selector;
        }
        (bool ok, bytes4 err) = callAs(
            caller, address(vault), abi.encodeCall(IVault.requestDeposit, (assets, receiver, statement))
        );
        expectOutcome(ok, err, want, "requestDeposit");
        if (ok) {
            Request memory r = vault.requestAt(vault.requestCount());
            assertEq(vault.requestCount(), b.requests + 1);
            assertEq(r.owner, receiver);
            assertTrue(r.kind == RequestKind.Deposit);
            assertEq(r.amount, assets);
            assertGt(
                r.earliest, vm.getBlockTimestamp(), "a deposit could be priced at a valuation already past"
            );
            assertEq(vault.escrow(), escrow + assets, "the deposit's money is not in escrow");
        }
        consistent(b, nobody());
    }

    function testFuzz_requestRedeem(
        uint8 stage,
        uint8 pick,
        address random,
        uint256 shares,
        uint8 to,
        address other
    ) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        address receiver = pickCaller(to, other);
        uint256 held = vault.balanceOf(caller);
        shares = bound(shares, 0, held * 2 + 1);
        World memory b = look(caller, receiver);
        bytes4 want;
        if (isClosed(b)) want = IVault.WrongState.selector;
        else if (shares == 0 || receiver == address(0)) want = IVault.TooSmall.selector;
        else if (shares > held) want = PANIC;
        (bool ok, bytes4 err) =
            callAs(caller, address(vault), abi.encodeCall(IVault.requestRedeem, (shares, receiver)));
        expectOutcome(ok, err, want, "requestRedeem");
        if (ok) {
            assertEq(vault.balanceOf(caller), held - shares, "the shares didn't leave the holder");
            Request memory r = vault.requestAt(vault.requestCount());
            assertEq(r.owner, receiver);
            assertEq(r.amount, shares);
            assertTrue(r.kind == RequestKind.Redeem);
        }
        consistent(b, nobody());
    }

    function testFuzz_recordValuation(uint8 stage, uint8 pick, address random, uint8 offset) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        vm.warp(nextBoundary() + SLOT + offset % SLOT);
        World memory b = look(caller, address(0));
        bytes4 want;
        if (isClosed(b)) want = IVault.WrongState.selector;
        else if (offset % SLOT > 3) want = IVault.OutsideWindow.selector;
        else if (stage == PRICED) want = IVault.Unsettled.selector;
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.recordValuation, ()));
        expectOutcome(ok, err, want, "recordValuation");
        if (ok) assertEq(vault.valuationCount(), b.valuations + 1);
        consistent(b, nobody());
    }

    /// A vault on one chain takes no posts and has nothing to close.
    function testFuzz_postStation(
        uint8 stage,
        uint8 pick,
        address random,
        uint64 time,
        uint256 chain,
        uint256 mark,
        uint256 key
    ) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        StationPost memory p;
        p.mark = mark;
        p.exit = mark;
        p.transit = new Transit[](0);
        key = bound(key, 1, 2 ** 128);
        bytes memory signature = signed(key, keccak256(abi.encode(time, chain)));
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) =
            callAs(caller, address(vault), abi.encodeCall(IVault.postStation, (time, chain, p, signature)));
        expectOutcome(ok, err, IVault.NotAllowed.selector, "postStation");
        consistent(b, nobody());
    }

    function testFuzz_closeValuation(uint8 stage, uint8 pick, address random, uint64 time) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.closeValuation, (time)));
        expectOutcome(ok, err, IVault.NotAllowed.selector, "closeValuation");
        consistent(b, nobody());
    }

    /// Anyone may settle, any number at a time; it never refuses, it only
    /// ever pays the requests' owners, and it never unsettles anything.
    function testFuzz_settle(uint8 stage, uint8 pick, address random, uint256 max) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.settle, (max)));
        expectOutcome(ok, err, bytes4(0), "settle");
        consistent(b, only(alice, bob, carol));
    }

    function testFuzz_settleClaim(uint8 stage, uint8 pick, address random, uint256 id) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        id = id % 2 == 0 ? vault.claimCount() : bound(id, 0, vault.claimCount() + 1);
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.settleClaim, (id)));
        if (id == 0 || id > b.claims) {
            expectOutcome(ok, err, PANIC, "settleClaim, no such claim");
        } else {
            refusedFor(
                ok,
                err,
                errs(IVault.NotAllowed.selector, IVault.WrongState.selector, IVault.NotYet.selector),
                "settleClaim"
            );
            if (isClosed(b)) assertFalse(ok, "a closed vault served a claim by a close");
        }
        consistent(b, only(bob, author, platform));
    }

    function testFuzz_freeze(uint8 stage, uint8 pick, address random, uint256 id, uint32 wait) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        id = id % 2 == 0 ? vault.claimCount() : bound(id, 0, vault.claimCount() + 1);
        skip(bound(wait, 0, 10 days));
        World memory b = look(caller, address(0));
        bytes4 want;
        if (id == 0 || id > b.claims) {
            want = PANIC;
        } else {
            Claim memory c = vault.claimAt(id);
            if (
                c.state != ClaimState.Pending && c.state != ClaimState.Waiting && c.state != ClaimState.Queued
            ) {
                want = IVault.NotAllowed.selector;
            } else if (vm.getBlockTimestamp() < c.bookedAt + 7 days) {
                want = IVault.NotYet.selector;
            } else if (
                (b.state != VaultState.Open && b.state != VaultState.WindingDown) || vault.haltedAt() != 0
            ) {
                want = IVault.WrongState.selector;
            }
        }
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.freeze, (id)));
        expectOutcome(ok, err, want, "freeze");
        if (ok) {
            assertTrue(vault.state() == VaultState.Frozen);
            assertEq(vault.haltedAt(), vm.getBlockTimestamp());
        }
        consistent(b, nobody());
    }

    function testFuzz_freezeUnits(uint8 stage, uint8 pick, address random, uint8 who, uint32 wait) public {
        stage = arrange(stage);
        // Units on a vault already halted though still live (an evicted
        // retirement): it is stopping anyway.
        if (stage == UNITS && wait % 3 == 0) {
            vm.prank(author);
            vault.retire();
            skip(7 days);
            vault.evict();
        }
        address caller = pickCaller(pick, random);
        address holder = pickCaller(who, random);
        skip(bound(wait, 0, 10 days));
        World memory b = look(caller, address(0));
        (uint256 held,) = vault.unitsOf(holder);
        uint64 since = vault.unitsSince(holder);
        bytes4 want;
        held;
        if (since == 0) {
            want = IVault.NotAllowed.selector;
        } else if (vm.getBlockTimestamp() < since + 7 days) {
            want = IVault.NotYet.selector;
        } else if ((b.state != VaultState.Open && b.state != VaultState.WindingDown) || vault.haltedAt() != 0)
        {
            want = IVault.WrongState.selector;
        }
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.freezeUnits, (holder)));
        expectOutcome(ok, err, want, "freezeUnits");
        if (ok) {
            assertTrue(vault.state() == VaultState.Frozen);
            assertEq(vault.haltedAt(), vm.getBlockTimestamp());
        }
        consistent(b, nobody());
    }

    /// The fallback is its owner's alone: anyone else is refused, whatever
    /// the request.
    function testFuzz_takeProRata(uint8 stage, uint8 pick, address random, uint256 id, uint32 wait) public {
        arrange(stage);
        // Half the time the newest request, and then often its owner.
        id = id % 2 == 0 ? vault.requestCount() : bound(id, 0, vault.requestCount() + 1);
        address caller = pick >= 128 && id != 0 && id <= vault.requestCount()
            ? vault.requestAt(id).owner
            : pickCaller(pick, random);
        skip(bound(wait, 0, 3 days));
        World memory b = look(caller, address(0));
        Request memory r;
        if (id != 0 && id <= b.requests) r = vault.requestAt(id);
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.takeProRata, (id)));
        if (id == 0 || id > b.requests) {
            expectOutcome(ok, err, PANIC, "takeProRata, no such request");
        } else {
            if (caller != r.owner || r.done) {
                expectOutcome(ok, err, IVault.NotAllowed.selector, "takeProRata");
            }
            refusedFor(
                ok,
                err,
                errs(IVault.NotAllowed.selector, IVault.NotYet.selector, IVault.WrongState.selector),
                "takeProRata"
            );
            if (ok) assertTrue(vault.requestAt(id).done);
        }
        consistent(b, only(caller));
    }

    function testFuzz_claimInstead(uint8 stage, uint8 pick, address random, uint256 id, uint32 wait) public {
        arrange(stage);
        // Half the time the newest request, and then often its owner.
        id = id % 2 == 0 ? vault.requestCount() : bound(id, 0, vault.requestCount() + 1);
        address caller = pick >= 128 && id != 0 && id <= vault.requestCount()
            ? vault.requestAt(id).owner
            : pickCaller(pick, random);
        skip(bound(wait, 0, 3 days));
        World memory b = look(caller, address(0));
        Request memory r;
        if (id != 0 && id <= b.requests) r = vault.requestAt(id);
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.claimInstead, (id)));
        if (id == 0 || id > b.requests) {
            expectOutcome(ok, err, PANIC, "claimInstead, no such request");
        } else {
            if (caller != r.owner || r.done || r.kind != RequestKind.Redeem) {
                expectOutcome(ok, err, IVault.NotAllowed.selector, "claimInstead");
            }
            refusedFor(
                ok,
                err,
                errs(IVault.NotAllowed.selector, IVault.NotYet.selector, IVault.WrongState.selector),
                "claimInstead"
            );
            if (ok) {
                assertEq(vault.claimCount(), b.claims + 1);
                assertEq(
                    vault.claimAt(b.claims + 1).bookedAt, r.earliest, "a claim instead not booked from due"
                );
            }
        }
        consistent(b, nobody());
    }

    function testFuzz_settleClosed(uint8 stage, uint8 pick, address random, uint256 id) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        // Half the time, the request left for the closed vault.
        if (lateRequest != 0 && id % 2 == 0) id = lateRequest;
        else id = bound(id, 0, vault.requestCount() + 1);
        World memory b = look(caller, address(0));
        bytes4 want;
        if (!isClosed(b)) {
            want = IVault.WrongState.selector;
        } else if (id == 0 || id > b.requests) {
            want = PANIC;
        } else {
            Request memory r = vault.requestAt(id);
            if (r.done || r.kind == RequestKind.Claim) want = IVault.NotAllowed.selector;
        }
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.settleClosed, (id)));
        expectOutcome(ok, err, want, "settleClosed");
        address owner = id != 0 && id <= b.requests ? vault.requestAt(id).owner : address(0);
        if (ok) assertTrue(vault.requestAt(id).done);
        consistent(b, only(owner));
    }

    function testFuzz_takeProRataClaim(
        uint8 stage,
        uint8 pick,
        address random,
        uint256 id,
        uint32 wait
    ) public {
        arrange(stage);
        id = id % 2 == 0 ? vault.claimCount() : bound(id, 0, vault.claimCount() + 1);
        address caller = pick >= 128 && id != 0 && id <= vault.claimCount()
            ? vault.claimAt(id).owner
            : pickCaller(pick, random);
        skip(bound(wait, 0, 3 days));
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.takeProRataClaim, (id)));
        if (id == 0 || id > b.claims) {
            expectOutcome(ok, err, PANIC, "takeProRataClaim, no such claim");
        } else {
            if (caller != vault.claimAt(id).owner) {
                expectOutcome(ok, err, IVault.NotAllowed.selector, "takeProRataClaim");
            }
            refusedFor(
                ok,
                err,
                errs(IVault.NotAllowed.selector, IVault.NotYet.selector, IVault.WrongState.selector),
                "takeProRataClaim"
            );
            if (ok) assertTrue(vault.claimAt(id).state == ClaimState.Fallback);
        }
        consistent(b, only(caller));
    }

    /// A vault on one chain has no exit costs to report.
    function testFuzz_reportExitCost(
        uint8 stage,
        uint8 pick,
        address random,
        uint256 id,
        uint256 cost,
        address payTo,
        uint256 key
    ) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        id = bound(id, 1, vault.claimCount() + 1);
        key = bound(key, 1, 2 ** 128);
        World memory b = look(caller, payTo);
        bytes memory signature = signed(key, keccak256(abi.encode(id, cost, payTo)));
        (bool ok, bytes4 err) = callAs(
            caller, address(vault), abi.encodeCall(IVault.reportExitCost, (id, cost, payTo, signature))
        );
        expectOutcome(ok, err, id > b.claims ? PANIC : IVault.NotAllowed.selector, "reportExitCost");
        consistent(b, nobody());
    }

    function testFuzz_collectUnits(uint8 stage, uint8 pick, address random) public {
        arrange(stage);
        address caller = pick >= 128 ? alice : pickCaller(pick, random);
        (, uint256 claimable) = vault.unitsOf(caller);
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.collectUnits, ()));
        expectOutcome(ok, err, claimable == 0 ? IVault.Nothing.selector : bytes4(0), "collectUnits");
        if (ok) {
            assertEq(usd.balanceOf(caller), b.usdOf[12] + claimable, "collected other than what was owed");
        }
        consistent(b, only(caller));
    }

    function testFuzz_serveProRata(
        uint8 stage,
        uint8 pick,
        address random,
        uint8 whose,
        address other
    ) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        address holder = whose % 2 == 0 ? alice : pickCaller(whose, other);
        (uint256 units,) = vault.unitsOf(holder);
        World memory b = look(caller, holder);
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.serveProRata, (holder)));
        if (units == 0) expectOutcome(ok, err, IVault.Nothing.selector, "serveProRata, no units");
        refusedFor(ok, err, errs(IVault.Nothing.selector, IVault.NotYet.selector), "serveProRata");
        if (ok) {
            (uint256 left,) = vault.unitsOf(holder);
            assertEq(left, 0, "a served holder kept units");
        }
        consistent(b, nobody());
    }

    function testFuzz_redeemClosed(
        uint8 stage,
        uint8 pick,
        address random,
        uint256 shares,
        uint8 to,
        address other
    ) public {
        arrange(stage);
        address caller = pick >= 128 ? alice : pickCaller(pick, random);
        address receiver = pickCaller(to, other);
        uint256 held = vault.balanceOf(caller);
        shares = bound(shares, 0, held * 2 + 1);
        World memory b = look(caller, receiver);
        bytes4 want;
        if (!isClosed(b)) want = IVault.WrongState.selector;
        else if (receiver == address(0)) want = IVault.TooSmall.selector;
        else if (shares > held) want = PANIC;
        uint256 owed = isClosed(b) ? vault.cash() * shares / vault.totalSupply() : 0;
        (bool ok, bytes4 err) =
            callAs(caller, address(vault), abi.encodeCall(IVault.redeemClosed, (shares, receiver)));
        expectOutcome(ok, err, want, "redeemClosed");
        if (ok) assertEq(vault.balanceOf(caller), held - shares);
        if (ok && receiver != caller && receiver != address(vault)) {
            assertEq(
                usd.balanceOf(receiver), b.usdOf[13] + owed, "paid other than the shares' part of the cash"
            );
        }
        consistent(b, only(receiver));
    }

    function testFuzz_claimClosed(uint8 stage, uint8 pick, address random, uint256 id) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        id = id % 2 == 0 ? vault.claimCount() : bound(id, 0, vault.claimCount() + 1);
        World memory b = look(caller, address(0));
        bytes4 want;
        if (!isClosed(b)) {
            want = IVault.WrongState.selector;
        } else if (id == 0 || id > b.claims) {
            want = PANIC;
        } else {
            ClaimState s = vault.claimAt(id).state;
            if (s != ClaimState.Pending && s != ClaimState.Waiting && s != ClaimState.Queued) {
                want = IVault.NotAllowed.selector;
            }
        }
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.claimClosed, (id)));
        expectOutcome(ok, err, want, "claimClosed");
        if (ok) assertTrue(vault.claimAt(id).state == ClaimState.Paid);
        consistent(b, only(bob));
    }

    /// A payout the dollar refused is held for its owner, who alone takes
    /// it, once, whoever calls.
    function testFuzz_collectHeld(uint8 stage, uint8 pick, address random, bool refused) public {
        stage = arrange(stage);
        if (refused && (stage == WAITING || stage == PRICED)) {
            // Alice's withdrawal is priced while the dollar refuses her.
            vm.mockCallRevert(
                address(usd), abi.encodeWithSelector(IERC20.transfer.selector, alice), "blocked"
            );
            if (stage == WAITING) {
                vm.warp(nextBoundary());
                vault.recordValuation();
            }
            vault.settle(type(uint256).max);
            vm.clearMockedCalls();
            assertGt(vault.heldFor(alice), 0, "the refused payout was not held");
        }
        address caller = pick >= 128 ? alice : pickCaller(pick, random);
        uint256 held = vault.heldFor(caller);
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.collectHeld, ()));
        expectOutcome(ok, err, held == 0 ? IVault.Nothing.selector : bytes4(0), "collectHeld");
        if (ok) {
            assertEq(usd.balanceOf(caller), b.usdOf[12] + held, "collected other than what was held");
            assertEq(vault.heldFor(caller), 0);
            (ok, err) = callAs(caller, address(vault), abi.encodeCall(IVault.collectHeld, ()));
            expectOutcome(ok, err, IVault.Nothing.selector, "collectHeld, twice");
        }
        consistent(b, only(caller));
    }

    // The home station ------------------------------------------------------

    /// Only the home station draws cash, and only to itself.
    function testFuzz_toStation(uint8 stage, uint8 pick, address random, uint256 amount) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        amount = bound(amount, 0, vault.cash() + 1);
        World memory b = look(caller, address(0));
        uint256 cash = vault.cash();
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(Vault.toStation, (amount)));
        if (caller != address(station)) expectOutcome(ok, err, IVault.NotAllowed.selector, "toStation");
        refusedFor(
            ok,
            err,
            errs(IVault.NotAllowed.selector, IVault.WrongState.selector, IVault.BelowFloor.selector),
            "toStation"
        );
        if (ok) assertEq(vault.cash(), cash - amount);
        consistent(b, nobody());
    }

    function testFuzz_fromStation(uint8 stage, uint8 pick, address random, uint256 amount) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        amount = bound(amount, 0, 1_000_000e6);
        World memory b = look(caller, address(0));
        uint256 cash = vault.cash();
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(Vault.fromStation, (amount)));
        if (caller != address(station)) expectOutcome(ok, err, IVault.NotAllowed.selector, "fromStation");
        // The station never approves the vault outside its own sends home.
        refusedFor(
            ok,
            err,
            errs(IVault.NotAllowed.selector, IERC20Errors.ERC20InsufficientAllowance.selector),
            "fromStation"
        );
        if (ok) assertEq(amount, 0, "the vault took money the station didn't send");
        if (ok) assertEq(vault.cash(), cash);
        consistent(b, nobody());
    }

    // The tank --------------------------------------------------------------

    function testFuzz_fundTank(uint8 stage, uint8 pick, address random, uint256 assets, bool funded) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        assets = bound(assets, 0, 1_000_000e6);
        if (funded && caller != address(0)) {
            give(caller, assets);
            vm.prank(caller);
            usd.approve(address(vault), assets);
        }
        World memory b = look(caller, address(0));
        uint256 tank = vault.tank().balance;
        bytes4 want;
        if (usd.allowance(caller, address(vault)) < assets) {
            want = IERC20Errors.ERC20InsufficientAllowance.selector;
        } else if (usd.balanceOf(caller) < assets) {
            want = IERC20Errors.ERC20InsufficientBalance.selector;
        }
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.fundTank, (assets)));
        expectOutcome(ok, err, want, "fundTank");
        if (ok) assertEq(vault.tank().balance, tank + assets);
        consistent(b, nobody());
    }

    function executorBlessed() internal view returns (bool) {
        return attestation.isBlessedFor(executor, block.chainid, address(vault), bundle);
    }

    /// Anyone may send `keep`: with no signature it collects nothing; with
    /// one it collects only a total the vault's executor signed later than
    /// the last, and never more than the tank holds.
    function testFuzz_keep(
        uint8 stage,
        uint8 pick,
        address random,
        uint256 total,
        uint64 asOf,
        uint8 how,
        uint256 key
    ) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        total = bound(total, 0, 1000e6);
        asOf = uint64(bound(asOf, 0, vm.getBlockTimestamp() + 1 hours));
        key = bound(key, 1, 2 ** 128);
        bytes memory signature;
        if (how % 3 == 1) signature = signed(EXECUTOR_KEY, vault.usageDigest(total, asOf));
        else if (how % 3 == 2) signature = signed(key, vault.usageDigest(total, asOf));
        World memory b = look(caller, address(0));
        Tank memory t = vault.tank();
        bytes4 want;
        if (signature.length != 0) {
            if ((how % 3 == 2 && vm.addr(key) != executor) || !executorBlessed()) {
                want = IVault.NotBlessed.selector;
            } else if (total < t.usageSettled || asOf <= t.usageAsOf || asOf > vm.getBlockTimestamp()) {
                want = IVault.Stale.selector;
            }
        }
        (bool ok, bytes4 err) =
            callAs(caller, address(vault), abi.encodeCall(IVault.keep, (total, asOf, signature)));
        expectOutcome(ok, err, want, "keep");
        if (ok) {
            assertLe(usd.balanceOf(platform) - b.usdOf[8], t.balance, "charged more than the tank held");
            if (signature.length != 0) assertEq(vault.tank().usageSettled, total);
        }
        consistent(b, only(platform));
    }

    function testFuzz_withdrawTank(
        uint8 stage,
        uint8 pick,
        address random,
        uint256 assets,
        uint256 total,
        uint32 age
    ) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        total = bound(total, 0, 100e6);
        uint64 asOf = uint64(vm.getBlockTimestamp() - bound(age, 1, 1 hours));
        bytes memory signature = signed(EXECUTOR_KEY, vault.usageDigest(total, asOf));
        World memory b = look(caller, address(0));
        Tank memory t = vault.tank();
        bytes4 want;
        if (caller != author) want = IVault.NotAllowed.selector;
        else if (asOf + 10 minutes < vm.getBlockTimestamp()) want = IVault.Stale.selector;
        else if (!executorBlessed()) want = IVault.NotBlessed.selector;
        else if (total < t.usageSettled || asOf <= t.usageAsOf) want = IVault.Stale.selector;
        (bool ok, bytes4 err) = callAs(
            caller, address(vault), abi.encodeCall(IVault.withdrawTank, (assets, total, asOf, signature))
        );
        expectOutcome(ok, err, want, "withdrawTank");
        consistent(b, only(author, platform));
    }

    function testFuzz_requestTankWithdrawal(uint8 stage, uint8 pick, address random) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) =
            callAs(caller, address(vault), abi.encodeCall(IVault.requestTankWithdrawal, ()));
        expectOutcome(
            ok, err, caller == author ? bytes4(0) : IVault.NotAllowed.selector, "requestTankWithdrawal"
        );
        if (ok) assertEq(vault.tank().escapeRequestedAt, vm.getBlockTimestamp());
        consistent(b, nobody());
    }

    function testFuzz_withdrawTankUnsigned(
        uint8 stage,
        uint8 pick,
        address random,
        uint256 assets,
        bool asked,
        uint32 wait
    ) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        if (asked) {
            vm.prank(author);
            vault.requestTankWithdrawal();
        }
        skip(bound(wait, 0, 3 days));
        World memory b = look(caller, address(0));
        Tank memory t = vault.tank();
        bytes4 want;
        if (caller != author) {
            want = IVault.NotAllowed.selector;
        } else if (
            !isClosed(b)
                && (t.escapeRequestedAt == 0 || vm.getBlockTimestamp() < t.escapeRequestedAt + 1 days)
        ) {
            want = IVault.NotYet.selector;
        } else if (!isClosed(b) && vm.getBlockTimestamp() > t.escapeRequestedAt + 2 days) {
            want = IVault.NotAllowed.selector;
        }
        (bool ok, bytes4 err) =
            callAs(caller, address(vault), abi.encodeCall(IVault.withdrawTankUnsigned, (assets)));
        expectOutcome(ok, err, want, "withdrawTankUnsigned");
        if (ok) {
            uint256 paid = assets > t.balance ? t.balance : assets;
            assertEq(usd.balanceOf(author), b.usdOf[7] + paid, "the author was paid other than asked");
        }
        consistent(b, only(author));
    }

    function testFuzz_topUpFromShares(uint8 stage, uint8 pick, address random, uint256 shares) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        World memory b = look(caller, address(0));
        bytes4 want;
        if (caller != author) want = IVault.NotAllowed.selector;
        else if (isClosed(b)) want = IVault.WrongState.selector;
        else if (shares == 0) want = IVault.TooSmall.selector;
        (bool ok, bytes4 err) =
            callAs(caller, address(vault), abi.encodeCall(IVault.topUpFromShares, (shares)));
        expectOutcome(ok, err, want, "topUpFromShares");
        if (ok) assertTrue(vault.requestAt(vault.requestCount()).kind == RequestKind.TankFill);
        consistent(b, nobody());
    }

    function testFuzz_setTankKeep(uint8 stage, uint8 pick, address random, uint256 assets) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.setTankKeep, (assets)));
        expectOutcome(ok, err, caller == author ? bytes4(0) : IVault.NotAllowed.selector, "setTankKeep");
        if (ok) assertEq(vault.tank().keepAt, assets);
        consistent(b, nobody());
    }

    function testFuzz_setAllowances(uint8 stage, uint8 pick, address random, Allowances memory a) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.setAllowances, (a)));
        expectOutcome(ok, err, caller == author ? bytes4(0) : IVault.NotAllowed.selector, "setAllowances");
        if (ok) assertEq(abi.encode(vault.allowances()), abi.encode(a));
        consistent(b, nobody());
    }

    // Who may deposit -------------------------------------------------------

    function testFuzz_recordStatement(
        uint8 stage,
        uint8 pick,
        address random,
        uint8 whose,
        address other,
        uint256 key
    ) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        address depositor = pickCaller(whose, other);
        key = bound(key, 1, 2 ** 128);
        bool keyed = depositor == alice || depositor == bob || depositor == author || depositor == carol;
        uint256 signer = keyed && key % 2 == 0 ? (depositor == carol ? carolKey : keyOf(depositor)) : key;
        bytes memory signature = statementBy(signer, address(vault), depositor, vault.restrictedText());
        World memory b = look(caller, depositor);
        bool valid = vm.addr(signer) == depositor;
        (bool ok, bytes4 err) =
            callAs(caller, address(vault), abi.encodeCall(IVault.recordStatement, (depositor, signature)));
        expectOutcome(ok, err, valid ? bytes4(0) : IVault.BadSignature.selector, "recordStatement");
        if (ok) assertEq(vault.statementOf(depositor), keccak256(abi.encode(vault.restricted())));
        consistent(b, nobody());
    }

    function testFuzz_refreshRestricted(uint8 stage, uint8 pick, address random) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        uint256 length = vault.restricted().length;
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.refreshRestricted, ()));
        expectOutcome(ok, err, bytes4(0), "refreshRestricted");
        assertGe(vault.restricted().length, length, "the restricted list shrank");
        consistent(b, nobody());
    }

    // The author and the admin ----------------------------------------------

    function testFuzz_setIdleFloor(uint8 stage, uint8 pick, address random, uint16 bps) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        (uint16 floor,,) = vault.idleFloor();
        World memory b = look(caller, address(0));
        bytes4 want;
        if (caller != author) want = IVault.NotAllowed.selector;
        else if (bps > 10_000) want = IVault.NotAllowed.selector;
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.setIdleFloor, (bps)));
        expectOutcome(ok, err, want, "setIdleFloor");
        if (ok) {
            (uint16 now_, uint16 pending, uint64 at) = vault.idleFloor();
            if (bps >= floor) {
                assertEq(now_, bps, "a raised floor didn't apply at once");
            } else {
                assertEq(now_, floor, "a lowered floor applied at once");
                assertEq(pending, bps);
                assertEq(at, vm.getBlockTimestamp() + 1 days);
            }
        }
        consistent(b, nobody());
    }

    function testFuzz_retire(uint8 stage, uint8 pick, address random) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        World memory b = look(caller, address(0));
        bytes4 want;
        if (caller != author) want = IVault.NotAllowed.selector;
        else if (b.state != VaultState.Open) want = IVault.WrongState.selector;
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.retire, ()));
        expectOutcome(ok, err, want, "retire");
        if (ok) assertTrue(vault.state() == VaultState.WindingDown);
        consistent(b, nobody());
    }

    function killing(
        uint8 stage,
        uint8 pick,
        address random,
        bytes memory call,
        string memory what
    ) internal {
        arrange(stage);
        address caller = pickCaller(pick, random);
        World memory b = look(caller, address(0));
        bytes4 want;
        if (caller != admin) {
            want = IVault.NotAllowed.selector;
        } else if (
            b.state != VaultState.Activating && b.state != VaultState.Open
                && b.state != VaultState.WindingDown && b.state != VaultState.Frozen
        ) {
            want = IVault.WrongState.selector;
        }
        (bool ok, bytes4 err) = callAs(caller, address(vault), call);
        expectOutcome(ok, err, want, what);
        if (ok) {
            assertTrue(vault.state() == VaultState.Killed);
            assertGt(vault.haltedAt(), 0);
        }
        consistent(b, nobody());
    }

    function testFuzz_kill(uint8 stage, uint8 pick, address random) public {
        killing(stage, pick, random, abi.encodeCall(IVault.kill, ()), "kill");
    }

    function testFuzz_windDownSealed(uint8 stage, uint8 pick, address random) public {
        killing(stage, pick, random, abi.encodeCall(IVault.windDownSealed, ()), "windDownSealed");
    }

    // Lifecycle, open to anyone ---------------------------------------------

    /// A vault on one chain opened at activation: there is nothing to
    /// confirm and nothing to cancel.
    function testFuzz_confirmStations(uint8 stage, uint8 pick, address random) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.confirmStations, ()));
        expectOutcome(ok, err, IVault.WrongState.selector, "confirmStations");
        consistent(b, nobody());
    }

    function testFuzz_cancel(uint8 stage, uint8 pick, address random, uint32 wait) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        skip(bound(wait, 0, 10 days));
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.cancel, ()));
        expectOutcome(ok, err, IVault.WrongState.selector, "cancel");
        consistent(b, nobody());
    }

    function testFuzz_evict(uint8 stage, uint8 pick, address random, uint32 wait) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        uint256 waited = bound(wait, 0, 10 days);
        skip(waited);
        World memory b = look(caller, address(0));
        bytes4 want;
        if (b.state != VaultState.WindingDown || vault.haltedAt() != 0) want = IVault.WrongState.selector;
        else if (waited < 7 days) want = IVault.NotYet.selector;
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.evict, ()));
        expectOutcome(ok, err, want, "evict");
        if (ok) assertEq(vault.haltedAt(), vm.getBlockTimestamp());
        consistent(b, nobody());
    }

    /// Anyone may close a halted vault once a valuation after the halt found
    /// its stations empty and what it priced is settled; never before.
    function testFuzz_close(uint8 stage, uint8 pick, address random, bool emptied) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        if (emptied && stage == KILLED) {
            station.stop();
            station.unwind(1e18);
            value(vault);
        }
        World memory b = look(caller, address(0));
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.close, ()));
        if (vault.haltedAt() == 0 || isClosed(b)) {
            expectOutcome(ok, err, IVault.WrongState.selector, "close");
        } else if (emptied && stage == KILLED) {
            expectOutcome(ok, err, bytes4(0), "close, emptied");
        } else {
            expectOutcome(ok, err, IVault.NotYet.selector, "close, stations not empty");
        }
        if (ok) assertTrue(vault.state() == VaultState.Closed);
        consistent(b, nobody());
    }

    // Shares ----------------------------------------------------------------

    function testFuzz_shares_never_move_between_holders(
        uint8 stage,
        uint8 pick,
        address random,
        address to,
        uint256 amount
    ) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        World memory b = look(caller, to);
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(Vault.transfer, (to, amount)));
        expectOutcome(ok, err, IVault.NotTransferable.selector, "transfer");
        (ok, err) = callAs(caller, address(vault), abi.encodeCall(Vault.transferFrom, (caller, to, amount)));
        expectOutcome(ok, err, IVault.NotTransferable.selector, "transferFrom");
        (ok, err) = callAs(caller, address(vault), abi.encodeCall(Vault.approve, (to, amount)));
        expectOutcome(ok, err, IVault.NotTransferable.selector, "approve");
        assertEq(vault.allowance(caller, to), 0);
        consistent(b, nobody());
    }
}

/// The parts of a vault only a vault that waits for another chain's posts
/// uses: opening a valuation, posts, the close, exit costs, and confirming
/// or cancelling the stations.
contract VaultAcrossChainsExternalFunctionsTest is TwoChainVault, Calls {
    function setUp() public override {
        super.setUp();
        depositTwo(alice, 100_000e6);
    }

    function pickCaller(uint8 pick, address random) internal view returns (address) {
        uint8 p = pick % 6;
        if (p == 0) return executor;
        if (p == 1) return unwinder;
        if (p == 2) return alice;
        if (p == 3) return author;
        if (p == 4) return admin;
        return random;
    }

    function blessedHere(address who) internal view returns (bool) {
        return attestation.isBlessedFor(who, block.chainid, address(vault), bundle);
    }

    /// Only the vault's own keys open a valuation that waits for posts.
    function testFuzz_recordValuation(uint8 pick, address random, uint8 offset) public {
        address caller = pickCaller(pick, random);
        uint256 t = vm.getBlockTimestamp() + 2 * SLOT;
        vm.warp(t - (t % SLOT) + offset % SLOT);
        uint256 count = vault.valuationCount();
        bytes4 want;
        if (!blessedHere(caller)) want = IVault.NotBlessed.selector;
        else if (offset % SLOT > 3) want = IVault.OutsideWindow.selector;
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.recordValuation, ()));
        expectOutcome(ok, err, want, "recordValuation");
        assertEq(vault.valuationCount(), ok ? count + 1 : count);
    }

    /// Anyone may carry a post; only one signed by the vault's executor or
    /// unwinder, for the open valuation's time and the other chain, inside
    /// the window, counts.
    function testFuzz_postStation(
        uint8 pick,
        address random,
        uint256 key,
        uint8 signer,
        uint64 time,
        uint256 chain,
        uint64 mark,
        bool opened
    ) public {
        address caller = pickCaller(pick, random);
        uint64 at;
        if (opened) at = open();
        key = signer % 3 == 0 ? EXECUTOR_KEY : signer % 3 == 1 ? UNWINDER_KEY : bound(key, 1, 2 ** 128);
        if (time % 2 == 0) time = at;
        if (chain % 2 == 0) chain = OTHER;
        StationPost memory p = postOf(mark, mark);
        p.asOf = vault.openedAt(vault.valuationCount() - 1) + SLOT;
        bytes memory signature = signed(key, vault.postDigest(time, chain, p));
        Valuation memory v = latest();
        bytes4 want;
        if (v.time != time || v.status != ValuationStatus.Open) want = IVault.NotAllowed.selector;
        else if (chain != OTHER) want = IVault.NotAllowed.selector;
        else if (!blessedHere(vm.addr(key))) want = IVault.NotBlessed.selector;
        (bool ok, bytes4 err) =
            callAs(caller, address(vault), abi.encodeCall(IVault.postStation, (time, chain, p, signature)));
        expectOutcome(ok, err, want, "postStation");
        if (ok) assertEq(latest().postsMissing, 0);
    }

    function testFuzz_closeValuation(
        uint8 pick,
        address random,
        uint64 time,
        uint16 after_,
        bool opened
    ) public {
        address caller = pickCaller(pick, random);
        uint64 at = opened ? open() : uint64(vm.getBlockTimestamp());
        if (time % 2 == 0) time = at;
        vm.warp(at + bound(after_, 0, 2 * DEADLINE));
        Valuation memory v = latest();
        bytes4 want;
        if (v.time != time || v.status != ValuationStatus.Open) want = IVault.NotAllowed.selector;
        else if (vm.getBlockTimestamp() <= time + DEADLINE) want = IVault.NotYet.selector;
        (bool ok, bytes4 err) = callAs(caller, address(vault), abi.encodeCall(IVault.closeValuation, (time)));
        expectOutcome(ok, err, want, "closeValuation");
        // Nobody posted: it prices nothing.
        if (ok) assertTrue(latest().status == ValuationStatus.Void);
    }

    /// An exit cost is believed only from the vault's unwinder, for a claim
    /// still open, up to the deployment's limit, only ever raised, and paid
    /// to somebody.
    function testFuzz_reportExitCost(
        uint8 pick,
        address random,
        uint256 key,
        uint8 signer,
        uint256 cost,
        address payTo,
        uint256 earlier
    ) public {
        vm.prank(executor);
        station.pull(90_000e6);
        uint256 shares = vault.balanceOf(alice);
        vm.prank(alice);
        vault.requestRedeem(shares, alice);
        valueWith(0, 0);
        uint256 id = vault.claimCount();
        assertEq(id, 1, "alice's withdrawal was not a claim");
        earlier = bound(earlier, 0, MAX_EXIT_COST);
        if (earlier != 0) {
            vault.reportExitCost(
                id, earlier, operator, signed(UNWINDER_KEY, vault.exitCostDigest(id, earlier, operator))
            );
        }
        address caller = pickCaller(pick, random);
        key = signer % 3 == 0 ? EXECUTOR_KEY : signer % 3 == 1 ? UNWINDER_KEY : bound(key, 1, 2 ** 128);
        cost = bound(cost, 0, 2 * MAX_EXIT_COST);
        bytes memory signature = signed(key, vault.exitCostDigest(id, cost, payTo));
        bytes4 want;
        if (cost > MAX_EXIT_COST || cost <= earlier || payTo == address(0)) want = IVault.NotAllowed.selector;
        else if (key != UNWINDER_KEY) want = IVault.NotBlessed.selector;
        uint256 aliceUsd = usd.balanceOf(alice);
        (bool ok, bytes4 err) = callAs(
            caller, address(vault), abi.encodeCall(IVault.reportExitCost, (id, cost, payTo, signature))
        );
        expectOutcome(ok, err, want, "reportExitCost");
        (uint256 recorded, address to) = vault.exitCostOf(id);
        assertEq(recorded, ok ? cost : earlier, "the exit cost recorded is not the last one believed");
        if (ok) assertEq(to, payTo);
        assertEq(usd.balanceOf(alice), aliceUsd, "reporting a cost paid somebody");
    }

    /// Only the executor bridges, only over a declared bridge, only to the
    /// other chain the vault trades on, never while a valuation waits for
    /// posts, and the money goes only to the bridge.
    function testFuzz_station_bridge(
        uint8 pick,
        address random,
        bytes32 name,
        uint256 toChain,
        uint256 amount,
        bool valuing
    ) public {
        vm.prank(executor);
        station.pull(50_000e6);
        if (valuing) open();
        address caller = pickCaller(pick, random);
        if (uint256(name) % 3 != 0) name = "local-bridge";
        if (toChain % 3 != 0) toChain = toChain % 3 == 1 ? OTHER : block.chainid;
        amount = bound(amount, 0, 100_000e6);
        uint256 sends = station.sendCount();
        uint256 carried = usd.balanceOf(address(bridge));
        uint256 idle = usd.balanceOf(address(station));
        uint256 callerUsd = usd.balanceOf(caller);
        bytes4 want;
        if (!blessedHere(caller) || attestation.keyInfo(caller).kind != ImageKind.Executor) {
            want = Station.NotBlessed.selector;
        } else if (toChain == block.chainid) {
            want = Station.NoRoute.selector;
        } else if (valuing) {
            want = Station.NotAllowed.selector;
        } else if (toChain != OTHER) {
            want = Station.NoRoute.selector;
        } else if (name != "local-bridge") {
            want = Station.NotDeclared.selector;
        } else if (amount > idle) {
            want = IERC20Errors.ERC20InsufficientBalance.selector;
        }
        (bool ok, bytes4 err) =
            callAs(caller, address(station), abi.encodeCall(Station.bridge, (name, toChain, amount, "")));
        expectOutcome(ok, err, want, "bridge");
        assertEq(station.sendCount(), ok ? sends + 1 : sends, "a send was recorded other than by sending");
        assertEq(
            usd.balanceOf(address(bridge)),
            ok ? carried + amount : carried,
            "the bridge took other than the send"
        );
        if (caller != address(bridge)) {
            assertEq(usd.balanceOf(caller), callerUsd, "a bridge send paid the caller");
        }
    }

    /// A vault waiting for its other stations opens only on its executor's
    /// word, and anyone may cancel it once its activation window has passed,
    /// which sends the author's money back to the author alone.
    function testFuzz_confirmStations_and_cancel(
        uint8 pick,
        address random,
        uint32 wait,
        bool confirmFirst
    ) public {
        uint256 id = submitAs(author);
        bytes32 second = keccak256(abi.encode("bundle", id));
        registerTwoChains(id, second);
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(second, 1));
        Vault waiting = activate(id, allowancesCosting(2e6), MINIMUM_TANK);
        assertTrue(waiting.state() == VaultState.Activating);
        address caller = pickCaller(pick, random);

        if (confirmFirst) {
            bool blessed = caller == executor;
            if (blessed) blessExecutorKey(executor, address(waiting), second);
            (bool ok, bytes4 err) =
                callAs(caller, address(waiting), abi.encodeCall(IVault.confirmStations, ()));
            expectOutcome(ok, err, blessed ? bytes4(0) : IVault.NotBlessed.selector, "confirmStations");
            if (ok) assertTrue(waiting.state() == VaultState.Open);
        }
        uint256 waited = bound(wait, 0, 6 days);
        skip(waited);
        uint256 authorUsd = usd.balanceOf(author);
        uint256 callerUsd = usd.balanceOf(caller);
        bytes4 want;
        if (waiting.state() != VaultState.Activating) want = IVault.WrongState.selector;
        else if (waited < 3 days) want = IVault.NotYet.selector;
        (bool done, bytes4 why) = callAs(caller, address(waiting), abi.encodeCall(IVault.cancel, ()));
        expectOutcome(done, why, want, "cancel");
        if (done) {
            assertTrue(waiting.state() == VaultState.Cancelled);
            assertEq(
                usd.balanceOf(author), authorUsd + MINIMUM_TANK, "the author didn't get their money back"
            );
            assertEq(usd.balanceOf(address(waiting)), 0);
            if (caller != author) assertEq(usd.balanceOf(caller), callerUsd, "cancelling paid the caller");
        }
    }
}
