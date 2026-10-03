// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

// The rest of the platform's state-changing functions, called with random
// arguments by random callers. The coverage list for these and the vault's
// is at the top of ExternalFunctions.t.sol.

import {AttestationRegistry} from "../../src/AttestationRegistry.sol";
import {AuthorProfiles} from "../../src/AuthorProfiles.sol";
import {LocalBridge} from "../../src/LocalBridge.sol";
import {Prices} from "../../src/Prices.sol";
import {ReviewRegistry} from "../../src/ReviewRegistry.sol";
import {Station} from "../../src/Station.sol";
import {StationFactory} from "../../src/StationFactory.sol";
import {StrategyRegistry} from "../../src/StrategyRegistry.sol";
import {Vault} from "../../src/Vault.sol";
import {VaultFactory} from "../../src/VaultFactory.sol";
import {ImageKind, KeyInfo} from "../../src/interfaces/IAttestationRegistry.sol";
import {Funding, ILocalVenue, OrderRequest, Quote} from "../../src/interfaces/ILocalVenue.sol";
import {PricePeriod, UnitPrices} from "../../src/interfaces/IPrices.sol";
import {Review} from "../../src/interfaces/IReviewRegistry.sol";
import {StationSpec} from "../../src/interfaces/IStation.sol";
import {Strategy} from "../../src/interfaces/IStrategyRegistry.sol";
import {Fees, IVault, Tank, Timers, VaultState} from "../../src/interfaces/IVault.sol";
import {IVaultFactory, VaultConfig} from "../../src/interfaces/IVaultFactory.sol";
import {ClaimBase} from "../ClaimBase.sol";
import {Fixture} from "../Fixture.sol";
import {Calls} from "./Calls.sol";
import {IERC20Errors} from "openzeppelin/interfaces/draft-IERC6093.sol";

/// Activation and the factory's terms.
contract VaultFactoryExternalFunctionsTest is Fixture, Calls {
    /// Only a ready strategy's author activates it, once, on the terms they
    /// read, with a tank of at least the minimum and their own statement;
    /// the tank is the only money that moves, from the author to the vault.
    function testFuzz_activate(
        uint8 which,
        uint8 pick,
        address random,
        uint256 tank,
        uint16 floor,
        uint8 flags
    ) public {
        // The author read the terms in force, has the tank to hand, and
        // signed their statement.
        bool termsRead = flags & 1 != 0;
        bool funded = flags & 2 != 0;
        bool signs = flags & 4 != 0;
        (uint256 readyId,) = ready();
        (Vault opened,) = openVault();
        uint256 id = which % 3 == 0 ? readyId : which % 3 == 1 ? opened.strategyId() : 1000 + which;
        address caller = pick % 4 == 0 ? author : pick % 4 == 1 ? alice : pick % 4 == 2 ? admin : random;
        tank = bound(tank, 0, 1000e6);
        if (funded && caller != address(0)) {
            give(caller, tank);
            vm.prank(caller);
            usd.approve(address(factory), tank);
        }
        bytes32 terms = termsRead ? factory.terms() : keccak256(abi.encode(random, tank));
        (, string memory text) = factory.restrictedFor(id);
        bytes memory statement =
            signs ? statementBy(authorKey, factory.predictVault(id), author, text) : bytes("");
        Strategy memory record = strategies.strategyOf(id);

        bytes4 want;
        if (factory.vaultOf(id) != address(0)) {
            want = VaultFactory.VaultExists.selector;
        } else if (!termsRead) {
            want = VaultFactory.TermsChanged.selector;
        } else if (record.registeredAt == 0 || !reviews.hasReview(id)) {
            want = VaultFactory.NotReady.selector;
        } else if (caller != record.author) {
            want = VaultFactory.NotTheAuthor.selector;
        } else if (tank < MINIMUM_TANK) {
            want = VaultFactory.TankTooSmall.selector;
        } else if (floor > 10_000) {
            want = VaultFactory.OutOfRange.selector;
        } else if (usd.allowance(caller, address(factory)) < tank) {
            want = IERC20Errors.ERC20InsufficientAllowance.selector;
        } else if (usd.balanceOf(caller) < tank) {
            want = IERC20Errors.ERC20InsufficientBalance.selector;
        } else if (!signs) {
            want = IVault.BadSignature.selector;
        }

        uint256 callerUsd = usd.balanceOf(caller);
        (bool ok, bytes4 err) = callAs(
            caller,
            address(factory),
            abi.encodeCall(
                VaultFactory.activate, (id, allowancesCosting(2e6), floor, tank, 0, statement, terms)
            )
        );
        expectOutcome(ok, err, want, "activate");
        if (ok) {
            Vault vault = Vault(factory.vaultOf(id));
            assertEq(address(vault), factory.predictVault(id));
            assertTrue(vault.state() == VaultState.Open);
            assertEq(usd.balanceOf(address(vault)), tank, "the vault holds other than the tank");
            assertEq(usd.balanceOf(caller), callerUsd - tank, "the author paid other than the tank");
            assertEq(vault.tank().balance + vault.cash(), tank);
            assertEq(vault.author(), caller);
        } else {
            assertEq(usd.balanceOf(caller), callerUsd);
        }
    }

    /// The factory's terms as the handbook bounds them, restated: the fee
    /// ceilings, and for every timer its floor, its ceiling of a year, and
    /// its order against the timers it must outlast.
    function inRange(VaultConfig memory c) internal pure returns (bool) {
        Timers memory t = c.timers;
        return c.holdingBps <= 500 && c.transactionBps <= 100 && c.exitBandBps <= 1000
            && uint256(c.authorBps) + c.platformBps <= 5000 && c.deadAssets != 0 && c.minimumTank > c.deadAssets
            && c.minimumDeposit != 0 && c.minimumDeposit <= 100e6 && c.maxExitCost <= 1000e6
            && t.slotInterval != 0 && t.slotWindow < t.slotInterval && t.slotLead > t.slotWindow
            && t.postDeadline > t.slotWindow && t.transitTimeout > t.postDeadline && t.usageFreshness != 0
            && t.usageFreshness <= 1 days && t.fallbackAfter >= 1 days && t.freezeAfter > t.fallbackAfter
            && t.tankEscapeDelay >= 1 days && t.floorDelay >= 1 days && t.haltGrace >= 10 minutes
            && t.windDown >= 7 days && t.activationWindow >= 1 days && t.slotInterval <= 365 days
            && t.slotLead <= 365 days && t.postDeadline <= 365 days && t.transitTimeout <= 365 days
            && t.fallbackAfter <= 365 days && t.usageFreshness <= 365 days && t.tankEscapeDelay <= 365 days
            && t.floorDelay <= 365 days && t.haltGrace <= 365 days && t.freezeAfter <= 365 days
            && t.windDown <= 365 days && t.activationWindow <= 365 days;
    }

    /// One timer at a time moved to just inside or just outside one of its
    /// bounds, the rest as the fixture has them: the factory refuses the
    /// terms, at deployment and on a change, exactly when a bound is
    /// crossed.
    function testFuzz_every_timer_bound_is_enforced_on_its_own(
        uint8 which,
        uint8 side,
        uint32 nudge
    ) public {
        VaultConfig memory c = vaultConfig();
        Timers memory t = c.timers;
        uint256 w = which % 13;
        bool atFloor = side % 2 == 0;
        bool above = side % 4 < 2;
        uint256 n = bound(nudge, 0, 1 hours);
        // Each timer's floor and ceiling, given the others as the fixture
        // sets them.
        uint64[13] memory floors = [
            uint64(t.slotWindow + 1),
            t.slotWindow + 1,
            0,
            t.slotWindow + 1,
            t.postDeadline + 1,
            1 days,
            1,
            1 days,
            1 days,
            10 minutes,
            t.fallbackAfter + 1,
            7 days,
            1 days
        ];
        uint64 lesser = t.slotLead < t.slotInterval ? t.slotLead : t.slotInterval;
        uint64[13] memory ceilings = [
            uint64(365 days),
            365 days,
            lesser - 1,
            t.transitTimeout - 1,
            365 days,
            t.freezeAfter - 1,
            1 days,
            365 days,
            365 days,
            365 days,
            365 days,
            365 days,
            365 days
        ];
        uint256 edge = atFloor ? floors[w] : ceilings[w];
        uint64 v = uint64(above ? edge + n : edge > n ? edge - n : 0);
        if (w == 0) t.slotInterval = v;
        else if (w == 1) t.slotLead = v;
        else if (w == 2) t.slotWindow = v;
        else if (w == 3) t.postDeadline = v;
        else if (w == 4) t.transitTimeout = v;
        else if (w == 5) t.fallbackAfter = v;
        else if (w == 6) t.usageFreshness = v;
        else if (w == 7) t.tankEscapeDelay = v;
        else if (w == 8) t.floorDelay = v;
        else if (w == 9) t.haltGrace = v;
        else if (w == 10) t.freezeAfter = v;
        else if (w == 11) t.windDown = v;
        else t.activationWindow = v;
        c.timers = t;
        bytes4 want = inRange(c) ? bytes4(0) : VaultFactory.OutOfRange.selector;
        (bool ok, bytes4 err) = callAs(admin, address(factory), abi.encodeCall(VaultFactory.setConfig, (c)));
        expectOutcome(ok, err, want, "setConfig at a timer's bound");
        assertEq(ok, inRange(c), "a timer crossed its bound unrefused, or was refused inside it");
        VaultFactory.Deps memory deps = VaultFactory.Deps({
            asset: factory.asset(),
            attestation: attestation,
            strategies: strategies,
            reviews: reviews,
            prices: prices,
            stationFactory: stations,
            vaultImplementation: factory.vaultImplementation(),
            admin: admin,
            platform: platform,
            previous: IVaultFactory(address(0))
        });
        if (inRange(c)) {
            new VaultFactory(deps, c, chainNames());
        } else {
            vm.expectRevert(VaultFactory.OutOfRange.selector);
            new VaultFactory(deps, c, chainNames());
        }
    }

    /// Only the admin sets the terms, only inside their ceilings, and a
    /// change never reaches a vault already open.
    function testFuzz_setConfig(uint8 pick, address random, VaultConfig memory c, bool sane) public {
        (Vault opened,) = openVault();
        Fees memory fees = opened.fees();
        if (sane) {
            // Mostly inside the ceilings, so the change goes through too.
            c.holdingBps %= 600;
            c.authorBps %= 3000;
            c.platformBps %= 3000;
            c.transactionBps %= 120;
            c.exitBandBps %= 1100;
            c.minimumDeposit = bound(c.minimumDeposit, 0, 110e6);
            c.deadAssets = bound(c.deadAssets, 0, 1e6);
            c.minimumTank = bound(c.minimumTank, 0, 100e6);
            c.maxExitCost = bound(c.maxExitCost, 0, 1100e6);
            c.timers = timers();
        }
        address caller = pick % 2 == 0 ? admin : random;
        bytes4 want;
        if (caller != admin) want = VaultFactory.NotAdmin.selector;
        else if (!inRange(c)) want = VaultFactory.OutOfRange.selector;
        bytes32 terms = factory.terms();
        (bool ok, bytes4 err) = callAs(caller, address(factory), abi.encodeCall(VaultFactory.setConfig, (c)));
        expectOutcome(ok, err, want, "setConfig");
        assertEq(factory.terms(), ok ? keccak256(abi.encode(c)) : terms);
        assertEq(abi.encode(opened.fees()), abi.encode(fees), "a change of terms reached an open vault");
    }
}

/// A vault's home station, and the factory that makes stations.
contract StationExternalFunctionsTest is ClaimBase, Calls {
    uint256 internal constant SECOND_KEY = 0xE8ED;
    address internal second = vm.addr(SECOND_KEY);

    function setUp() public override {
        super.setUp();
        depositAs(vault, alice, 100_000e6);
        // 10k idle at the station, 20k cash and half a BTC at the venue.
        deploySpot(60_000e6, 50_000e6, 0.5e18);
        // A second instance of the same executor image, for handovers.
        blessExecutorKey(second, address(vault), bundle);
    }

    uint8 internal constant RUNNING = 0;
    uint8 internal constant HALTED = 1;
    uint8 internal constant STOPPED = 2;

    function arrange(uint8 stage) internal returns (uint8) {
        stage %= 3;
        if (stage != RUNNING) {
            vm.prank(admin);
            vault.kill();
            skip(HALT_GRACE);
            if (stage == STOPPED) station.stop();
        }
        return stage;
    }

    function pickCaller(uint8 pick, address random) internal view returns (address) {
        uint8 p = pick % 6;
        if (p == 0) return executor;
        if (p == 1) return second;
        if (p == 2) return alice;
        if (p == 3) return address(vault);
        if (p == 4) return admin;
        return random == address(station) ? address(uint160(random) + 1) : random;
    }

    function isExecutor(address key) internal view returns (bool) {
        return attestation.isBlessedFor(key, block.chainid, address(vault), bundle)
            && attestation.keyInfo(key).kind == ImageKind.Executor;
    }

    function pickDriver(uint256 d) internal pure returns (bytes32) {
        return d % 3 == 0 ? bytes32(keccak256(abi.encode(d))) : bytes32("local");
    }

    /// What every money-moving call leaves true: the vault holds what it
    /// owes, and no dollar left the vault, the station, the venue or the
    /// bridge for anyone at all.
    struct Books {
        address[] who;
        uint256[] usdOf;
    }

    function books(address caller) internal view returns (Books memory b) {
        b.who = new address[](8);
        b.who[0] = address(vault);
        b.who[1] = address(station);
        b.who[2] = address(venue);
        b.who[3] = address(bridge);
        b.who[4] = executor;
        b.who[5] = second;
        b.who[6] = alice;
        b.who[7] = caller;
        b.usdOf = new uint256[](8);
        for (uint256 i = 0; i < 8; i++) {
            b.usdOf[i] = usd.balanceOf(b.who[i]);
        }
    }

    function nothingLeft(Books memory b) internal view {
        uint256 before;
        uint256 afterwards;
        for (uint256 i = 0; i < 8; i++) {
            if (seenBefore(b.who, i, b.who[i])) continue;
            uint256 now_ = usd.balanceOf(b.who[i]);
            before += b.usdOf[i];
            afterwards += now_;
            if (i >= 4) assertLe(now_, b.usdOf[i], "a station call paid somebody");
        }
        assertEq(afterwards, before, "money left the vault's own accounts");
        Tank memory t = vault.tank();
        assertGe(
            usd.balanceOf(address(vault)),
            vault.cash() + t.balance + vault.escrow(),
            "the vault owes more than it holds"
        );
    }

    // The factory -----------------------------------------------------------

    function testFuzz_factory_initialize(
        uint8 pick,
        address random,
        address a,
        address b,
        address c,
        address d
    ) public {
        address caller = pick % 2 == 0 ? address(this) : random;
        address implementation = stations.implementation();
        (bool ok, bytes4 err) =
            callAs(caller, address(stations), abi.encodeCall(StationFactory.initialize, (a, b, c, d)));
        expectOutcome(
            ok,
            err,
            caller == address(this)
                ? StationFactory.AlreadyInitialized.selector
                : StationFactory.NotInitializer.selector,
            "initialize"
        );
        assertEq(stations.implementation(), implementation);
    }

    /// Only the vault factory makes a home station, and asking again for a
    /// vault's station returns it as it was.
    function testFuzz_factory_createHome(
        uint8 pick,
        address random,
        bool existing,
        address other,
        uint8 drivers,
        uint16 band
    ) public {
        address caller = pick % 2 == 0 ? address(factory) : random;
        address forVault = existing ? address(vault) : other;
        bytes32[] memory list = new bytes32[](1 + drivers % 2);
        for (uint256 i = 0; i < list.length; i++) {
            list[i] = drivers % 4 < 2 ? bytes32("local") : bytes32(keccak256(abi.encode(i)));
        }
        bool fresh = !stations.exists(block.chainid, forVault);
        bytes4 want;
        if (caller != address(factory)) want = StationFactory.NotVaultFactory.selector;
        else if (band > 1000) want = StationFactory.OutOfRange.selector;
        else if (fresh && list.length == 2 && list[0] == list[1]) want = Station.NotAllowed.selector;
        else if (fresh && list[0] != "local") want = Station.NotApproved.selector;
        bytes32 specBefore = keccak256(abi.encode(station.spec()));
        (bool ok, bytes4 err) = callAs(
            caller,
            address(stations),
            abi.encodeCall(
                StationFactory.createHome, (forVault, bundle, list, new bytes32[](0), new uint256[](0), band)
            )
        );
        expectOutcome(ok, err, want, "createHome");
        assertEq(keccak256(abi.encode(station.spec())), specBefore, "an existing station was changed");
        if (ok) {
            assertTrue(stations.exists(block.chainid, forVault));
            assertTrue(Station(stations.stationOf(block.chainid, forVault)).isHome());
        }
    }

    // The station -----------------------------------------------------------

    function testFuzz_initialize(uint8 pick, address random, bytes32 driver) public {
        address caller = pickCaller(pick, random);
        bytes32[] memory list = new bytes32[](1);
        list[0] = driver;
        StationSpec memory spec = StationSpec({
            vaultChain: block.chainid,
            vault: caller,
            bundleHash: driver,
            drivers: list,
            bridges: new bytes32[](0),
            chains: new uint256[](0),
            exitBandBps: 0
        });
        (bool ok, bytes4 err) = callAs(caller, address(station), abi.encodeCall(Station.initialize, (spec)));
        expectOutcome(ok, err, Station.AlreadyInitialized.selector, "initialize");
        (ok, err) = callAs(caller, stations.implementation(), abi.encodeCall(Station.initialize, (spec)));
        expectOutcome(ok, err, Station.AlreadyInitialized.selector, "initialize, the implementation");
        assertEq(station.spec().vault, address(vault));
    }

    /// Anyone may ask; only the registry's word counts, one executor at a
    /// time, and while the key in place is still blessed only the key
    /// taking over can make the handover.
    function testFuzz_approveKey(
        uint8 stage,
        uint8 pick,
        address random,
        uint256 d,
        uint8 k,
        address otherKey
    ) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        bytes32 driver = pickDriver(d);
        address key = k % 3 == 0 ? executor : k % 3 == 1 ? second : otherKey;
        address current = station.approvedKey("local");
        Books memory b = books(caller);
        bytes4 want;
        if (stage == STOPPED) {
            want = Station.AlreadyStopped.selector;
        } else if (driver != "local") {
            want = Station.NotDeclared.selector;
        } else if (!isExecutor(key)) {
            want = Station.NotBlessed.selector;
        } else if (current != address(0) && current != key && caller != key && isExecutor(current)) {
            want = Station.NotAllowed.selector;
        }
        (bool ok, bytes4 err) =
            callAs(caller, address(station), abi.encodeCall(Station.approveKey, (driver, key)));
        expectOutcome(ok, err, want, "approveKey");
        if (ok) {
            assertEq(station.approvedKey("local"), key);
            assertTrue(venue.isAgent(address(station), key));
            if (current != key) {
                assertFalse(venue.isAgent(address(station), current), "the key replaced still trades");
            }
        }
        nothingLeft(b);
    }

    /// A passer-by can't cut off a key whose blessing stands, and anyone
    /// can withdraw one that has none, or any once the station stopped.
    function testFuzz_revokeKey(
        uint8 stage,
        uint8 pick,
        address random,
        uint256 d,
        uint8 k,
        address otherKey
    ) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        bytes32 driver = pickDriver(d);
        address key = k % 3 == 0 ? executor : k % 3 == 1 ? second : otherKey;
        Books memory b = books(caller);
        bytes4 want;
        if (driver != "local") {
            want = Station.NotDeclared.selector;
        } else if (
            stage != STOPPED && isExecutor(key)
                && !attestation.isDriverRevoked(driver, station.pinnedDriver(driver))
        ) {
            want = Station.StillBlessed.selector;
        }
        (bool ok, bytes4 err) =
            callAs(caller, address(station), abi.encodeCall(Station.revokeKey, (driver, key)));
        expectOutcome(ok, err, want, "revokeKey");
        if (ok) {
            assertFalse(venue.isAgent(address(station), key), "a revoked key still trades");
            if (key != address(0)) assertTrue(station.approvedKey("local") != key);
        }
        nothingLeft(b);
    }

    function executorOnly(uint8 stage, address caller) internal view returns (bytes4) {
        if (stage == STOPPED) return Station.AlreadyStopped.selector;
        if (!isExecutor(caller)) return Station.NotBlessed.selector;
        return bytes4(0);
    }

    function testFuzz_pull(uint8 stage, uint8 pick, address random, uint256 amount) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        amount = bound(amount, 0, vault.cash() + 1);
        Books memory b = books(caller);
        uint256 cash = vault.cash();
        (bool ok, bytes4 err) = callAs(caller, address(station), abi.encodeCall(Station.pull, (amount)));
        bytes4 want = executorOnly(stage, caller);
        if (want != bytes4(0)) expectOutcome(ok, err, want, "pull");
        else refusedFor(ok, err, errs(IVault.WrongState.selector, IVault.BelowFloor.selector), "pull");
        if (ok) assertEq(vault.cash(), cash - amount);
        nothingLeft(b);
    }

    function testFuzz_push(uint8 stage, uint8 pick, address random, uint256 amount) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        uint256 idle = usd.balanceOf(address(station));
        amount = bound(amount, 0, idle * 2);
        Books memory b = books(caller);
        uint256 cash = vault.cash();
        bytes4 want = executorOnly(stage, caller);
        if (want == bytes4(0) && amount > idle) want = IERC20Errors.ERC20InsufficientBalance.selector;
        (bool ok, bytes4 err) = callAs(caller, address(station), abi.encodeCall(Station.push, (amount)));
        expectOutcome(ok, err, want, "push");
        if (ok) assertEq(vault.cash(), cash + amount, "money sent home was not counted as cash");
        nothingLeft(b);
    }

    function testFuzz_deposit(uint8 stage, uint8 pick, address random, uint256 d, uint256 amount) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        bytes32 driver = pickDriver(d);
        uint256 idle = usd.balanceOf(address(station));
        amount = bound(amount, 0, idle * 2);
        Books memory b = books(caller);
        bytes4 want = executorOnly(stage, caller);
        if (want == bytes4(0) && driver != "local") want = Station.NotDeclared.selector;
        else if (want == bytes4(0) && amount > idle) want = IERC20Errors.ERC20InsufficientBalance.selector;
        (bool ok, bytes4 err) =
            callAs(caller, address(station), abi.encodeCall(Station.deposit, (driver, amount)));
        expectOutcome(ok, err, want, "deposit");
        nothingLeft(b);
    }

    function testFuzz_withdraw(uint8 stage, uint8 pick, address random, uint256 d, uint256 amount) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        bytes32 driver = pickDriver(d);
        int256 cashAtVenue = venue.cashOf(address(station));
        amount = bound(amount, 0, uint256(cashAtVenue) * 2);
        Books memory b = books(caller);
        bytes4 want = executorOnly(stage, caller);
        if (want == bytes4(0) && driver != "local") {
            want = Station.NotDeclared.selector;
        } else if (want == bytes4(0) && amount > uint256(cashAtVenue)) {
            want = ILocalVenueErrors.WouldGoNegative.selector;
        }
        (bool ok, bytes4 err) =
            callAs(caller, address(station), abi.encodeCall(Station.withdraw, (driver, amount)));
        expectOutcome(ok, err, want, "withdraw");
        nothingLeft(b);
    }

    /// A station whose vault trades on its own chain alone has nowhere to
    /// bridge to.
    function testFuzz_bridge(
        uint8 stage,
        uint8 pick,
        address random,
        bytes32 name,
        uint256 toChain,
        uint256 amount
    ) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        if (toChain % 3 == 0) toChain = 999;
        if (uint256(name) % 2 == 0) name = "local-bridge";
        Books memory b = books(caller);
        bytes4 want = executorOnly(stage, caller);
        if (want == bytes4(0)) want = Station.NoRoute.selector;
        (bool ok, bytes4 err) =
            callAs(caller, address(station), abi.encodeCall(Station.bridge, (name, toChain, amount, "")));
        expectOutcome(ok, err, want, "bridge");
        nothingLeft(b);
    }

    /// Anyone stops the home station, once, after the vault halted and its
    /// grace passed; never before.
    function testFuzz_stop(uint8 stage, uint8 pick, address random, uint32 wait) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        if (stage == RUNNING && wait % 2 == 0) {
            vm.prank(admin);
            vault.kill();
        }
        skip(bound(wait, 0, 2 * HALT_GRACE));
        uint64 halted = vault.haltedAt();
        Books memory b = books(caller);
        bytes4 want;
        if (halted == 0 || vm.getBlockTimestamp() < halted + HALT_GRACE) want = Station.NotHalted.selector;
        else if (stage == STOPPED) want = Station.AlreadyStopped.selector;
        (bool ok, bytes4 err) = callAs(caller, address(station), abi.encodeWithSignature("stop()"));
        expectOutcome(ok, err, want, "stop");
        if (ok) {
            assertTrue(station.stopped());
            assertFalse(venue.isAgent(address(station), executor), "a stopped station's key still trades");
        }
        (ok, err) = callAs(
            caller,
            address(station),
            abi.encodeWithSignature("stop(bytes32,uint64,bytes)", bytes32("halted"), halted, bytes(""))
        );
        expectOutcome(ok, err, Station.IsHome.selector, "stop, carried to the home station");
        nothingLeft(b);
    }

    /// Only the vault and its keys unwind a part; anyone unwinds all of a
    /// stopped station; what comes out goes to the vault alone.
    function testFuzz_unwind(uint8 stage, uint8 pick, address random, uint256 fraction) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        if (fraction % 4 == 0) fraction = 1e18;
        else fraction = bound(fraction, 0, 1.1e18);
        Books memory b = books(caller);
        uint256 cash = vault.cash();
        // Once stopped, a fraction is closed only for the vault itself; the
        // rest is a full unwind, open to anyone.
        bool permitted = caller == address(vault) || (stage != STOPPED && isExecutor(caller))
            || (stage == STOPPED && fraction == 1e18);
        bytes4 want;
        if (fraction == 0 || fraction > 1e18 || !permitted) want = Station.NotAllowed.selector;
        (bool ok, bytes4 err) = callAs(caller, address(station), abi.encodeCall(Station.unwind, (fraction)));
        expectOutcome(ok, err, want, "unwind");
        if (ok) assertGe(vault.cash(), cash, "an unwind took cash from the vault");
        nothingLeft(b);
    }

    /// A stopped station's unwind by name: anyone, once stopped; it never
    /// takes cash from the vault.
    function testFuzz_unwindStopped(uint8 stage, uint8 pick, address random, uint8 which) public {
        stage = arrange(stage);
        address caller = pickCaller(pick, random);
        bytes32[] memory names = new bytes32[](which % 3);
        for (uint256 i = 0; i < names.length; i++) {
            names[i] = which % 5 == 0 ? bytes32("elsewhere") : bytes32("local");
        }
        Books memory b = books(caller);
        uint256 cash = vault.cash();
        bytes4 want;
        if (stage != STOPPED) want = Station.NotHalted.selector;
        else if (which % 5 == 0 && names.length != 0) want = Station.NotDeclared.selector;
        (bool ok, bytes4 err) =
            callAs(caller, address(station), abi.encodeCall(Station.unwindStopped, (names)));
        expectOutcome(ok, err, want, "unwindStopped");
        if (ok) assertGe(vault.cash(), cash, "an unwind took cash from the vault");
        nothingLeft(b);
    }

    /// One driver's part of a stopped full unwind: only the station itself
    /// may call it.
    function testFuzz_closeAll(uint8 stage, uint8 pick, address random, bytes32 driver, uint16 band) public {
        arrange(stage);
        address caller = pickCaller(pick, random);
        Books memory b = books(caller);
        (bool ok, bytes4 err) =
            callAs(caller, address(station), abi.encodeCall(Station.closeAll, (driver, band)));
        expectOutcome(ok, err, Station.NotAllowed.selector, "closeAll");
        nothingLeft(b);
    }
}

/// The venue's errors, for the station tests.
interface ILocalVenueErrors {
    error WouldGoNegative();
    error NotOperator();
    error NotPermitted();
    error NoSuchMarket();
    error MarketExists();
}

/// A station on another chain: created only by the vault's executor with the
/// builder's word, and stopped only on a blessed key's word.
contract RemoteStationExternalFunctionsTest is Fixture, Calls {
    uint256 internal constant UNWINDER_KEY = 0xD1E;
    bytes32 internal constant UNWINDER_IMAGE = keccak256("unwinder image");
    uint256 internal constant VAULT_CHAIN = 8453;
    bytes32 internal constant BUNDLE = keccak256("bundle");

    address internal unwinder = vm.addr(UNWINDER_KEY);
    address internal remoteVault = makeAddr("remote vault");

    function setUp() public override {
        super.setUp();
        vm.prank(admin);
        attestation.proposeImage(UNWINDER_IMAGE, ImageKind.Unwinder);
        skip(TIMELOCK);
        blessServiceKey(BUILDER_IMAGE, builder);
        blessFor(EXECUTOR_IMAGE, executor);
        blessFor(UNWINDER_IMAGE, unwinder);
    }

    function blessFor(bytes32 image, address signer) internal {
        bytes32 leaf =
            keccak256(bytes.concat(keccak256(abi.encode(VAULT_CHAIN, remoteVault, BUNDLE, block.chainid))));
        bytes memory journal = journalFor(image, signer, leaf);
        attestation.bless(sealOf(journal), journal, VAULT_CHAIN, remoteVault, BUNDLE, new bytes32[](0));
    }

    function names(bytes32 a) internal pure returns (bytes32[] memory list) {
        list = new bytes32[](1);
        list[0] = a;
    }

    function specFor(
        uint256 vaultChain,
        address forVault,
        bytes32 driver,
        bytes32 bridgeName,
        uint16 band
    ) internal pure returns (StationSpec memory) {
        return StationSpec({
            vaultChain: vaultChain,
            vault: forVault,
            bundleHash: BUNDLE,
            drivers: names(driver),
            bridges: names(bridgeName),
            chains: new uint256[](0),
            exitBandBps: band
        });
    }

    function digestOf(StationSpec memory spec) internal view returns (bytes32) {
        return stations.stationDigest(
            spec.vaultChain, spec.bundleHash, spec.drivers, spec.bridges, spec.chains, spec.exitBandBps
        );
    }

    function testFuzz_factory_create(
        uint8 pick,
        address random,
        address other,
        uint8 flags,
        uint256 key,
        uint16 band
    ) public {
        // The spec is for this chain, for the vault the executor serves,
        // signed by the builder, with a known driver and a known bridge.
        bool homeChain = flags & 1 != 0;
        bool ownVault = flags & 2 != 0;
        bool builderSigns = flags & 4 != 0;
        bool knownDriver = flags & 8 != 0;
        bool knownBridge = flags & 16 != 0;
        address caller = pick % 3 == 0 ? executor : pick % 3 == 1 ? unwinder : random;
        StationSpec memory spec = specFor(
            homeChain ? block.chainid : VAULT_CHAIN,
            ownVault ? remoteVault : other,
            knownDriver ? bytes32("local") : bytes32("nowhere"),
            knownBridge ? bytes32("local-bridge") : bytes32("nobridge"),
            band % 1200
        );
        key = bound(key, 1, 2 ** 128);
        bytes memory word = signed(builderSigns ? BUILDER_KEY : key, digestOf(spec));
        bool executorForIt = attestation.isBlessedFor(caller, spec.vaultChain, spec.vault, BUNDLE)
            && attestation.keyInfo(caller).kind == ImageKind.Executor;
        bytes4 want;
        if (homeChain) {
            want = StationFactory.WrongChain.selector;
        } else if (!executorForIt) {
            want = StationFactory.NotBlessed.selector;
        } else if (!builderSigns && !attestation.isBlessedAs(vm.addr(key), ImageKind.Builder)) {
            want = StationFactory.NotBlessed.selector;
        } else if (spec.exitBandBps > 1000) {
            want = StationFactory.OutOfRange.selector;
        } else if (!knownDriver || !knownBridge) {
            // The station pins this chain's approvals as it is created.
            want = Station.NotApproved.selector;
        }
        (bool ok, bytes4 err) =
            callAs(caller, address(stations), abi.encodeCall(StationFactory.create, (spec, word)));
        expectOutcome(ok, err, want, "create");
        if (ok) {
            Station remote = Station(stations.stationOf(spec.vaultChain, spec.vault));
            assertFalse(remote.isHome());
            assertEq(remote.spec().vault, spec.vault);
            // Asking again returns the same station.
            vm.prank(caller);
            assertEq(stations.create(spec, word), address(remote));
        }
    }

    /// A stop carried from the vault's chain needs the word of a key blessed
    /// for the vault, as executor or unwinder; it stops once.
    function testFuzz_carried_stop(
        uint8 pick,
        address random,
        uint8 signer,
        uint256 key,
        bytes32 reason,
        uint64 at
    ) public {
        StationSpec memory spec = specFor(VAULT_CHAIN, remoteVault, "local", "local-bridge", 100);
        bytes memory word = signed(BUILDER_KEY, digestOf(spec));
        vm.prank(executor);
        Station remote = Station(stations.create(spec, word));
        key = signer % 3 == 0 ? EXECUTOR_KEY : signer % 3 == 1 ? UNWINDER_KEY : bound(key, 1, 2 ** 128);
        address caller = pick % 2 == 0 ? random : executor;
        bytes memory signature = signed(key, remote.stopDigest(reason, at));
        bool blessed = attestation.isBlessedFor(vm.addr(key), VAULT_CHAIN, remoteVault, BUNDLE);
        (bool ok, bytes4 err) = callAs(
            caller,
            address(remote),
            abi.encodeWithSignature("stop(bytes32,uint64,bytes)", reason, at, signature)
        );
        expectOutcome(ok, err, blessed ? bytes4(0) : Station.NotBlessed.selector, "carried stop");
        assertEq(remote.stopped(), ok);
        if (ok) {
            (ok, err) = callAs(
                caller,
                address(remote),
                abi.encodeWithSignature("stop(bytes32,uint64,bytes)", reason, at, signature)
            );
            expectOutcome(ok, err, Station.AlreadyStopped.selector, "carried stop, twice");
        }
        (ok, err) = callAs(caller, address(remote), abi.encodeWithSignature("stop()"));
        expectOutcome(ok, err, Station.NotHome.selector, "stop, on another chain");
    }
}

/// The registries, publishing, the price list and the profiles.
contract RegistryExternalFunctionsTest is Fixture, Calls {
    function pickCaller(uint8 pick, address random) internal view returns (address) {
        uint8 p = pick % 5;
        if (p == 0) return admin;
        if (p == 1) return author;
        if (p == 2) return reviewer;
        if (p == 3) return builder;
        return random;
    }

    // The attestation registry ----------------------------------------------

    /// Only the admin sets the timelock, never past a year and never under
    /// the floor fixed at deployment; a raise applies at once, a cut waits
    /// out the timelock it cuts.
    function testFuzz_setTimelock(
        uint8 pick,
        address random,
        uint64 value_,
        uint64 floor,
        bool stepTwice
    ) public {
        floor = uint64(bound(floor, 0, 30 days));
        AttestationRegistry registry =
            new AttestationRegistry(admin, verifier, floor + 1 days, floor, BLESSING_TTL);
        address caller = pickCaller(pick, random);
        value_ = uint64(bound(value_, 0, 400 days));
        uint64 current = registry.timelock();
        bytes4 want;
        if (caller != admin) want = AttestationRegistry.NotAdmin.selector;
        else if (value_ > 365 days) want = AttestationRegistry.TooLong.selector;
        else if (value_ < floor) want = AttestationRegistry.TooShort.selector;
        (bool ok, bytes4 err) =
            callAs(caller, address(registry), abi.encodeCall(AttestationRegistry.setTimelock, (value_)));
        expectOutcome(ok, err, want, "setTimelock");
        if (ok && value_ >= current) {
            assertEq(registry.timelock(), value_, "a raise didn't apply at once");
        } else if (ok) {
            assertEq(registry.timelock(), current, "a cut applied at once");
            skip(current);
            assertEq(registry.timelock(), value_);
        }
        if (stepTwice) skip(400 days);
        assertGe(registry.timelock(), floor, "the timelock went under its floor");
    }

    /// Every allowlist change is the admin's alone. A proposal never counts
    /// before its timelock has passed; a revocation counts at once.
    function testFuzz_admin_only(
        uint8 pick,
        address random,
        uint8 which,
        bytes32 name,
        address implementation,
        uint8 kind
    ) public {
        address caller = pickCaller(pick, random);
        which %= 12;
        if (uint256(name) % 3 == 0) {
            name = which < 2 ? EXECUTOR_IMAGE : which < 4 ? PROJECT : bytes32("local");
        }
        if (uint256(uint160(implementation)) % 3 == 0) implementation = address(driver);
        bytes memory call;
        if (which == 0) call = abi.encodeCall(AttestationRegistry.proposeImage, (name, ImageKind(kind % 5)));
        else if (which == 1) call = abi.encodeCall(AttestationRegistry.revokeImage, (name));
        else if (which == 2) call = abi.encodeCall(AttestationRegistry.proposeProject, (name));
        else if (which == 3) call = abi.encodeCall(AttestationRegistry.revokeProject, (name));
        else if (which == 4) call = abi.encodeCall(AttestationRegistry.proposeVerifier, (implementation));
        else if (which == 5) call = abi.encodeCall(AttestationRegistry.retireVerifier, (implementation));
        else if (which == 6) call = abi.encodeCall(AttestationRegistry.proposeDriver, (name, implementation));
        else if (which == 7) call = abi.encodeCall(AttestationRegistry.revokeDriver, (name, implementation));
        else if (which == 8) call = abi.encodeCall(AttestationRegistry.proposeBridge, (name, implementation));
        else if (which == 9) call = abi.encodeCall(AttestationRegistry.revokeBridge, (name, implementation));
        else if (which == 10) call = abi.encodeCall(AttestationRegistry.withdrawDriverProposal, (name));
        else call = abi.encodeCall(AttestationRegistry.withdrawBridgeProposal, (name));

        bool imageBefore = attestation.isImageAllowed(name);
        bool projectBefore = attestation.isProjectAllowed(name);
        bool verifierBefore = attestation.isVerifierAllowed(implementation);
        address tradingBefore = attestation.tradingDriver(name);
        address bridgeBefore = attestation.bridgeOf(name);
        (bool ok, bytes4 err) = callAs(caller, address(attestation), call);
        if (caller != admin) {
            expectOutcome(
                ok, err, AttestationRegistry.NotAdmin.selector, "an allowlist change by anyone else"
            );
            return;
        }
        refusedFor(
            ok,
            err,
            errs(
                AttestationRegistry.AlreadyProposed.selector,
                AttestationRegistry.NoKind.selector,
                AttestationRegistry.NoImplementation.selector
            ),
            "an allowlist change"
        );
        if (!ok) return;
        // A proposal counts for nothing yet; a revocation counts already.
        if (which == 0) {
            assertEq(attestation.isImageAllowed(name), imageBefore, "an image counted before its timelock");
        }
        if (which == 1) assertFalse(attestation.isImageAllowed(name), "a revoked image still counts");
        if (which == 2) {
            assertEq(attestation.isProjectAllowed(name), projectBefore, "a project counted early");
        }
        if (which == 3) assertFalse(attestation.isProjectAllowed(name), "a revoked project still counts");
        if (which == 4) {
            assertEq(attestation.isVerifierAllowed(implementation), verifierBefore, "a checker counted early");
        }
        if (which == 5) {
            assertFalse(attestation.isVerifierAllowed(implementation), "a retired checker still counts");
        }
        if (which == 6) assertEq(attestation.tradingDriver(name), tradingBefore, "a driver counted early");
        if (which == 7) {
            assertTrue(attestation.isDriverRevoked(name, implementation), "a revoked driver still trades");
            assertTrue(attestation.tradingDriver(name) != implementation, "a revoked driver is still pinned");
        }
        if (which == 8) assertEq(attestation.bridgeOf(name), bridgeBefore, "a bridge counted early");
        if (which == 9) {
            assertTrue(attestation.isBridgeRevoked(name, implementation), "a revoked bridge still carries");
        }
        // Withdrawing a waiting proposal leaves what is in force alone.
        if (which == 10) {
            assertEq(attestation.tradingDriver(name), tradingBefore, "a withdrawal changed the driver");
        }
        if (which == 11) {
            assertEq(attestation.bridgeOf(name), bridgeBefore, "a withdrawal changed the bridge");
        }
    }

    /// What an instance attests to, and where its key is to serve.
    struct Proof {
        bytes32 digest;
        bool projectOk;
        address signer;
        uint8 target;
        uint256 vaultChain;
        address forVault;
        bytes32 hash;
        bool honestRoot;
        uint64 issuedAt;
        bool attested;
        bytes journal;
        bytes seal;
    }

    /// `flags`: the project is allowed, the instance named this target
    /// honestly, and the checker's key signed it.
    function proofOf(
        uint8 image,
        uint8 whose,
        address signer,
        uint8 target,
        int32 issued,
        uint8 flags,
        uint256 key
    ) internal view returns (Proof memory p) {
        p.digest = image % 4 == 0
            ? EXECUTOR_IMAGE
            : image % 4 == 1 ? BUILDER_IMAGE : image % 4 == 2 ? REVIEWER_IMAGE : keccak256(abi.encode(image));
        p.projectOk = flags & 1 != 0;
        p.honestRoot = flags & 2 != 0;
        p.attested = flags & 4 != 0;
        p.signer = whose % 4 == 0 ? builder : whose % 4 == 1 ? reviewer : whose % 4 == 2 ? address(0) : signer;
        p.target = target % 3;
        if (p.target != 0) (p.vaultChain, p.hash) = (block.chainid, keccak256("a bundle"));
        if (p.target == 1) p.forVault = alice;
        bytes32 leaf =
            keccak256(bytes.concat(keccak256(abi.encode(p.vaultChain, p.forVault, p.hash, block.chainid))));
        p.issuedAt =
            uint64(int64(int256(vm.getBlockTimestamp())) + int64(bound(int256(issued), -100 hours, 1 hours)));
        p.journal = abi.encode(
            AttestationRegistry.Journal({
                imageDigest: p.digest,
                projectId: p.projectOk ? PROJECT : keccak256("another project"),
                signer: p.signer,
                vaultsRoot: p.honestRoot ? leaf : keccak256(abi.encode(leaf)),
                issuedAt: p.issuedAt
            })
        );
        if (p.attested) {
            p.seal = sealOf(p.journal);
        } else {
            (uint8 v, bytes32 r, bytes32 sig) = vm.sign(bound(key, 1, 2 ** 128), keccak256(p.journal));
            p.seal = abi.encodePacked(r, sig, v);
        }
    }

    /// Why `bless` must refuse `p`, in the order it checks.
    function refusalOf(Proof memory p) internal view returns (bytes4) {
        if (!p.attested) return AttestationRegistry.ProofRejected.selector;
        if (!attestation.isImageAllowed(p.digest)) return AttestationRegistry.ImageNotAllowed.selector;
        if (!p.projectOk) return AttestationRegistry.ProjectNotAllowed.selector;
        if (p.signer == address(0)) return AttestationRegistry.NoSigner.selector;
        ImageKind kind = attestation.imageOf(p.digest).kind;
        bool oneVault = kind == ImageKind.Executor || kind == ImageKind.Unwinder;
        if (oneVault ? p.target != 1 : p.target != 0) return AttestationRegistry.WrongKindForVault.selector;
        if (!p.honestRoot) return AttestationRegistry.NotAmongTheVaults.selector;
        if (p.issuedAt > vm.getBlockTimestamp() || p.issuedAt + BLESSING_TTL <= vm.getBlockTimestamp()) {
            return AttestationRegistry.Stale.selector;
        }
        KeyInfo memory known = attestation.keyInfo(p.signer);
        if (known.kind != ImageKind.None && (known.imageDigest != p.digest || known.projectId != PROJECT)) {
            return AttestationRegistry.NotThisInstance.selector;
        }
        return bytes4(0);
    }

    /// Anyone may bring a proof; a key is blessed only for an allowed image
    /// and project, for the work its kind does, for what the instance named,
    /// within its time to live; and a blessing only ever moves later.
    function testFuzz_bless(
        uint8 pick,
        address random,
        uint8 image,
        uint8 whose,
        address signer,
        uint8 target,
        int32 issued,
        uint8 flags,
        uint256 key
    ) public {
        address caller = pickCaller(pick, random);
        Proof memory p = proofOf(image, whose, signer, target, issued, flags, key);
        bytes4 want = refusalOf(p);
        uint64 before = attestation.blessedUntil(p.signer, p.vaultChain, p.forVault, p.hash);
        (bool ok, bytes4 err) = callAs(
            caller,
            address(attestation),
            abi.encodeCall(
                AttestationRegistry.bless,
                (p.seal, p.journal, p.vaultChain, p.forVault, p.hash, new bytes32[](0))
            )
        );
        expectOutcome(ok, err, want, "bless");
        uint64 until = attestation.blessedUntil(p.signer, p.vaultChain, p.forVault, p.hash);
        assertGe(until, before, "a blessing moved earlier");
        if (ok) {
            uint64 fresh = p.issuedAt + BLESSING_TTL;
            assertEq(until, fresh > before ? fresh : before);
            assertTrue(attestation.keyInfo(p.signer).kind == attestation.imageOf(p.digest).kind);
        }
    }

    // The strategy registry -------------------------------------------------

    struct Registration {
        uint256 id;
        address who;
        bytes32 bundleHash;
        bytes32[] restricted;
        bytes word;
        bytes clearance;
        bool built;
        bool sorted;
        bool builderSigns;
        bool reviewerClears;
        uint256 key;
    }

    /// `flags`: something was built, the list is sorted, the builder signed,
    /// the reviewer cleared it.
    function registrationOf(
        uint8 which,
        uint8 whose,
        address someone,
        uint8 flags,
        uint256 key
    ) internal returns (Registration memory g) {
        if (which % 3 == 0) g.id = submitAs(author);
        else if (which % 3 == 1) (g.id,) = ready();
        else g.id = 1000 + which;
        g.who = whose % 3 == 0 ? author : whose % 3 == 1 ? someone : address(0);
        g.built = flags & 1 != 0;
        g.sorted = flags & 2 != 0;
        g.builderSigns = flags & 4 != 0;
        g.reviewerClears = flags & 8 != 0;
        g.key = bound(key, 1, 2 ** 128);
        g.bundleHash = g.built ? keccak256("bundle") : bytes32(0);
        g.restricted = new bytes32[](2);
        (g.restricted[0], g.restricted[1]) =
            g.sorted ? (bytes32("SANCTIONED"), bytes32("US")) : (bytes32("US"), bytes32("SANCTIONED"));
        bytes32 digest = strategies.registrationDigest(
            g.id,
            g.who,
            keccak256("source"),
            g.bundleHash,
            keccak256("manifest"),
            oneChain(),
            new bytes32[](0),
            g.restricted
        );
        g.word = signed(g.builderSigns ? BUILDER_KEY : g.key, digest);
        g.clearance = signed(
            g.reviewerClears ? REVIEWER_KEY : g.key, strategies.clearanceDigest(g.id, keccak256("source"))
        );
    }

    function refusalOf(Registration memory g) internal view returns (bytes4) {
        if (strategies.isRegistered(g.id)) return StrategyRegistry.AlreadyRegistered.selector;
        if (!g.built) return StrategyRegistry.NothingBuilt.selector;
        if (g.who == address(0) || publishing.submissionOf(g.id).author != g.who) {
            return StrategyRegistry.NotSubmittedByTheAuthor.selector;
        }
        if (!g.sorted) return StrategyRegistry.NotSorted.selector;
        if (!g.builderSigns && !attestation.isBlessedAs(vm.addr(g.key), ImageKind.Builder)) {
            return StrategyRegistry.NotTheBuilder.selector;
        }
        if (!g.reviewerClears && !attestation.isBlessedAs(vm.addr(g.key), ImageKind.Reviewer)) {
            return StrategyRegistry.NotCleared.selector;
        }
        return bytes4(0);
    }

    /// Anyone may send a registration; it sticks only for the author who
    /// paid for the id, once, with something built, a sorted list, a
    /// blessed builder's signature and a blessed reviewer's go-ahead.
    function testFuzz_register(
        uint8 pick,
        address random,
        uint8 which,
        uint8 whose,
        address someone,
        uint8 flags,
        uint256 key
    ) public {
        address caller = pickCaller(pick, random);
        Registration memory g = registrationOf(which, whose, someone, flags, key);
        bytes4 want = refusalOf(g);
        bytes32 bundleBefore = strategies.bundleHashOf(g.id);
        (bool ok, bytes4 err) = callAs(
            caller,
            address(strategies),
            abi.encodeCall(
                StrategyRegistry.register,
                (
                    g.id,
                    g.who,
                    keccak256("source"),
                    g.bundleHash,
                    keccak256("manifest"),
                    oneChain(),
                    new bytes32[](0),
                    g.restricted,
                    g.word,
                    g.clearance
                )
            )
        );
        expectOutcome(ok, err, want, "register");
        if (ok) {
            assertEq(strategies.authorOf(g.id), g.who);
            assertEq(strategies.bundleHashOf(g.id), g.bundleHash);
        } else {
            assertEq(strategies.bundleHashOf(g.id), bundleBefore, "a refused registration changed the record");
        }
    }

    // The review registry ---------------------------------------------------

    /// A strategy submitted and registered, but not reviewed.
    function unreviewed() internal returns (uint256 id) {
        id = submitAs(author);
        register(id, author, keccak256(abi.encode("bundle", id)));
    }

    function pickStrategy(uint8 which) internal returns (uint256 id) {
        uint8 w = which % 4;
        if (w == 0) id = unreviewed();
        else if (w == 1) (id,) = ready();
        else if (w == 2) id = submitAs(author);
        else id = 1000 + which;
    }

    function aReview(
        uint256 id,
        bool rightBundle,
        bool rightSource,
        uint8 rating
    ) internal view returns (Review memory r) {
        r = reviewFor(rightBundle ? strategies.bundleHashOf(id) : keccak256("another bundle"), 1);
        if (!rightSource) r.sourceHash = keccak256("another source");
        r.ratings[rating % 6] = rating % 7 == 0 ? 0 : rating % 7 == 6 ? 5 : r.ratings[rating % 6];
    }

    function badRating(uint8 rating) internal pure returns (bool) {
        return rating % 7 == 0 || rating % 7 == 6;
    }

    function testFuzz_submitReview(
        uint8 pick,
        address random,
        uint8 which,
        bool rightBundle,
        bool rightSource,
        uint8 rating
    ) public {
        uint256 id = pickStrategy(which);
        address caller = pickCaller(pick, random);
        Review memory r = aReview(id, rightBundle, rightSource, rating);
        (Review memory stored,) = reviews.reviewOf(id);
        bytes4 want;
        if (!attestation.isBlessedAs(caller, ImageKind.Reviewer)) {
            want = ReviewRegistry.NotTheReviewer.selector;
        } else if (stored.reviewedAt != 0) {
            want = ReviewRegistry.AlreadyReviewed.selector;
        } else if (badRating(rating)) {
            want = ReviewRegistry.BadRating.selector;
        } else if (!strategies.isRegistered(id)) {
            want = ReviewRegistry.NotRegistered.selector;
        } else if (!rightBundle || !rightSource) {
            want = ReviewRegistry.WrongBundle.selector;
        }
        (bool ok, bytes4 err) =
            callAs(caller, address(reviews), abi.encodeCall(ReviewRegistry.submitReview, (id, r)));
        expectOutcome(ok, err, want, "submitReview");
        if (ok) assertTrue(reviews.hasReview(id));
        (Review memory now_,) = reviews.reviewOf(id);
        if (!ok) assertEq(now_.reviewedAt, stored.reviewedAt, "a refused review changed the record");
    }

    function testFuzz_submitRefusal(
        uint8 pick,
        address random,
        uint8 which,
        bool noBundle,
        bool hasSource,
        uint8 rating
    ) public {
        uint256 id = pickStrategy(which);
        address caller = pickCaller(pick, random);
        Review memory r = aReview(id, true, true, rating);
        if (noBundle) r.bundleHash = 0;
        if (!hasSource) r.sourceHash = 0;
        (Review memory stored,) = reviews.reviewOf(id);
        bytes4 want;
        if (!attestation.isBlessedAs(caller, ImageKind.Reviewer)) {
            want = ReviewRegistry.NotTheReviewer.selector;
        } else if (stored.reviewedAt != 0) {
            want = ReviewRegistry.AlreadyReviewed.selector;
        } else if (badRating(rating)) {
            want = ReviewRegistry.BadRating.selector;
        } else if (publishing.submissionOf(id).author == address(0)) {
            want = ReviewRegistry.NotSubmitted.selector;
        } else if (strategies.isRegistered(id)) {
            want = ReviewRegistry.AlreadyRegistered.selector;
        } else if (r.bundleHash != 0) {
            want = ReviewRegistry.WrongBundle.selector;
        } else if (!hasSource) {
            want = ReviewRegistry.NoSource.selector;
        }
        (bool ok, bytes4 err) =
            callAs(caller, address(reviews), abi.encodeCall(ReviewRegistry.submitRefusal, (id, r)));
        expectOutcome(ok, err, want, "submitRefusal");
        if (ok) {
            (, bool refused) = reviews.reviewOf(id);
            assertTrue(refused);
            assertFalse(reviews.hasReview(id), "a refused strategy reads as reviewed");
        }
    }

    // Publishing, prices and profiles ---------------------------------------

    /// Anyone pays the fee for the next id; the fee reaches the platform and
    /// nobody else.
    function testFuzz_submit(uint8 pick, address random, bytes32 upload, bool funded) public {
        address caller = pickCaller(pick, random);
        if (funded && caller != address(0)) {
            give(caller, PUBLISH_FEE);
            vm.prank(caller);
            usd.approve(address(publishing), PUBLISH_FEE);
        }
        uint256 count = publishing.count();
        uint256 platformUsd = usd.balanceOf(platform);
        uint256 callerUsd = usd.balanceOf(caller);
        bytes4 want;
        if (usd.allowance(caller, address(publishing)) < PUBLISH_FEE) {
            want = IERC20Errors.ERC20InsufficientAllowance.selector;
        } else if (callerUsd < PUBLISH_FEE) {
            want = IERC20Errors.ERC20InsufficientBalance.selector;
        }
        (bool ok, bytes4 err) =
            callAs(caller, address(publishing), abi.encodeWithSignature("submit(bytes32)", upload));
        expectOutcome(ok, err, want, "submit");
        assertEq(publishing.count(), ok ? count + 1 : count);
        if (ok && caller != platform) {
            assertEq(usd.balanceOf(platform), platformUsd + PUBLISH_FEE, "the fee didn't reach the platform");
            assertEq(usd.balanceOf(caller), callerUsd - PUBLISH_FEE);
            assertEq(publishing.submissionOf(count + 1).author, caller);
        }
    }

    /// Only the admin schedules prices, and a change only ever starts after
    /// the notice: the price in force now never moves.
    function testFuzz_schedule(uint8 pick, address random, UnitPrices memory next, uint32 wait) public {
        address caller = pickCaller(pick, random);
        skip(bound(wait, 0, 60 days));
        UnitPrices memory current = prices.current();
        uint256 length = prices.history().length;
        (bool ok, bytes4 err) = callAs(caller, address(prices), abi.encodeCall(Prices.schedule, (next)));
        expectOutcome(ok, err, caller == admin ? bytes4(0) : Prices.NotAdmin.selector, "schedule");
        assertEq(abi.encode(prices.current()), abi.encode(current), "the price in force moved without notice");
        PricePeriod[] memory history = prices.history();
        assertEq(history.length, ok ? length + 1 : length);
        if (ok) assertEq(history[length].effectiveFrom, vm.getBlockTimestamp() + PRICE_NOTICE);
    }

    /// Anyone sets their own profile; a handle is one person's at a time,
    /// and the handle they leave is free for someone else.
    function testFuzz_setProfile(
        uint8 pick,
        address random,
        uint8 handlePick,
        string memory other,
        uint8 links
    ) public {
        vm.prank(alice);
        profiles.setProfile("alice", "", new string[](0), new string[](0));
        address caller = pick % 3 == 0 ? alice : pick % 3 == 1 ? bob : random;
        string memory handle =
            handlePick % 4 == 0 ? "alice" : handlePick % 4 == 1 ? "bob" : handlePick % 4 == 2 ? "" : other;
        string[] memory labels = new string[](links % 3);
        string[] memory urls = new string[](links % 2 == 0 ? links % 3 : (uint256(links) + 1) % 3);
        string memory previous = profiles.profileOf(caller).handle;
        address owner = profiles.authorOfHandle(handle);
        bytes4 want;
        if (bytes(handle).length == 0) want = AuthorProfiles.EmptyHandle.selector;
        else if (labels.length != urls.length) want = AuthorProfiles.MismatchedLinks.selector;
        else if (owner != address(0) && owner != caller) want = AuthorProfiles.HandleTaken.selector;
        (bool ok, bytes4 err) = callAs(
            caller, address(profiles), abi.encodeCall(AuthorProfiles.setProfile, (handle, "", labels, urls))
        );
        expectOutcome(ok, err, want, "setProfile");
        if (ok) {
            assertEq(profiles.authorOfHandle(handle), caller);
            if (keccak256(bytes(previous)) != keccak256(bytes(handle)) && bytes(previous).length != 0) {
                assertEq(profiles.authorOfHandle(previous), address(0), "a handle left behind stayed taken");
            }
        } else {
            assertEq(profiles.authorOfHandle(handle), owner, "a refused profile moved a handle");
        }
    }
}

/// The stand-ins for a bridge and a venue: only the operator delivers or
/// moves markets, and only an account's master moves its dollars.
contract LocalStandInsExternalFunctionsTest is Fixture, Calls {
    function setUp() public override {
        super.setUp();
        // Alice's account at the venue, with bob as her agent.
        give(alice, 10_000e6);
        vm.startPrank(alice);
        usd.approve(address(venue), 10_000e6);
        venue.deposit(10_000e6);
        venue.approveAgent(bob, type(uint64).max);
        vm.stopPrank();
        // One send already delivered.
        vm.prank(operator);
        bridge.deliver(999, 1, alice, 1);
    }

    function pickCaller(uint8 pick, address random) internal view returns (address) {
        uint8 p = pick % 5;
        if (p == 0) return alice;
        if (p == 1) return bob;
        if (p == 2) return operator;
        if (p == 3) return admin;
        return random;
    }

    function testFuzz_bridge_carry(
        uint8 pick,
        address random,
        uint256 toChain,
        address recipient,
        uint256 amount,
        bool funded
    ) public {
        address caller = pickCaller(pick, random);
        amount = bound(amount, 0, 1_000_000e6);
        if (funded && caller != address(0)) {
            give(caller, amount);
            vm.prank(caller);
            usd.approve(address(bridge), amount);
        }
        uint256 sent = bridge.sent();
        uint256 held = usd.balanceOf(address(bridge));
        uint256 callerUsd = usd.balanceOf(caller);
        bytes4 want;
        if (usd.allowance(caller, address(bridge)) < amount) {
            want = IERC20Errors.ERC20InsufficientAllowance.selector;
        } else if (callerUsd < amount) {
            want = IERC20Errors.ERC20InsufficientBalance.selector;
        }
        (bool ok, bytes4 err) =
            callAs(caller, address(bridge), abi.encodeCall(LocalBridge.carry, (toChain, recipient, amount)));
        expectOutcome(ok, err, want, "carry");
        assertEq(bridge.sent(), ok ? sent + 1 : sent, "the send counter moved other than by one send");
        if (ok && caller != address(bridge)) {
            assertEq(usd.balanceOf(address(bridge)), held + amount);
            assertEq(usd.balanceOf(caller), callerUsd - amount);
        }
    }

    function testFuzz_bridge_deliver(
        uint8 pick,
        address random,
        uint256 fromChain,
        uint256 sequence,
        address to,
        uint256 amount
    ) public {
        address caller = pickCaller(pick, random);
        if (fromChain % 2 == 0) (fromChain, sequence) = (999, 1);
        amount = bound(amount, 0, 1_000_000e6);
        if (to == address(0)) to = bob;
        bool already = bridge.delivered(fromChain, sequence);
        uint256 toUsd = usd.balanceOf(to);
        bytes4 want;
        if (caller != operator) want = LocalBridge.NotOperator.selector;
        else if (already) want = LocalBridge.AlreadyDelivered.selector;
        (bool ok, bytes4 err) = callAs(
            caller, address(bridge), abi.encodeCall(LocalBridge.deliver, (fromChain, sequence, to, amount))
        );
        expectOutcome(ok, err, want, "deliver");
        assertEq(usd.balanceOf(to), ok ? toUsd + amount : toUsd);
        assertTrue(bridge.delivered(fromChain, sequence) == (ok || already));
    }

    /// Only the operator lists markets and moves prices and funding.
    function testFuzz_venue_operator_only(
        uint8 pick,
        address random,
        uint8 which,
        bytes32 market,
        uint256 price,
        int64 rate
    ) public {
        address caller = pickCaller(pick, random);
        if (uint256(market) % 2 == 0) market = SPOT;
        price = bound(price, 1, 1e12);
        which %= 5;
        bytes memory call;
        if (which == 0) {
            call = abi.encodeCall(ILocalVenue.listMarket, (market, false, price));
        } else if (which == 1) {
            call = abi.encodeCall(ILocalVenue.setPrices, (market, price, price, price, price));
        } else if (which == 2) {
            Quote[] memory quotes = new Quote[](1);
            quotes[0] = Quote({market: market, mark: price, oracle: price, bid: price, ask: price});
            call = abi.encodeCall(ILocalVenue.post, (quotes, new Funding[](0)));
        } else if (which == 3) {
            call = abi.encodeCall(ILocalVenue.accrueFunding, (market, rate));
        } else {
            call = abi.encodeCall(ILocalVenue.setFunding, (market, rate));
        }
        bool listed = venue.marketOf(market).id != bytes32(0);
        bytes4 want;
        if (caller != operator) want = ILocalVenueErrors.NotOperator.selector;
        else if (which == 0 && listed) want = ILocalVenueErrors.MarketExists.selector;
        else if (which != 0 && !listed) want = ILocalVenueErrors.NoSuchMarket.selector;
        uint256 markBefore = venue.marketOf(SPOT).mark;
        (bool ok, bytes4 err) = callAs(caller, address(venue), call);
        expectOutcome(ok, err, want, "a market operator's call");
        if (caller != operator) assertEq(venue.marketOf(SPOT).mark, markBefore);
    }

    /// One of an account holder's calls: approve or revoke an agent, put
    /// dollars in or take them out, or trade alice's account.
    function venueCall(
        uint8 which,
        uint256 amount,
        address agent,
        bool buy
    ) internal view returns (bytes memory) {
        if (which == 0) {
            return abi.encodeCall(ILocalVenue.approveAgent, (agent, uint64(vm.getBlockTimestamp() + 1 days)));
        }
        if (which == 1) return abi.encodeCall(ILocalVenue.revokeAgent, (agent));
        if (which == 2) return abi.encodeCall(ILocalVenue.deposit, (amount));
        if (which == 3) return abi.encodeCall(ILocalVenue.withdraw, (amount));
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0].market = SPOT;
        orders[0].isBuy = buy;
        orders[0].size = amount * 1e18 / PRICE;
        orders[0].limitPrice = PRICE;
        return abi.encodeCall(ILocalVenue.place, (alice, orders));
    }

    struct VenueAccount {
        int256 cash;
        uint256 worth;
        uint256 usd;
        bool bobAgent;
    }

    function accountOfAlice() internal view returns (VenueAccount memory a) {
        a.cash = venue.cashOf(alice);
        a.worth = venue.accountValue(alice);
        a.usd = usd.balanceOf(alice);
        a.bobAgent = venue.isAgent(alice, bob);
    }

    /// Whoever calls, nobody but alice moves alice's dollars, and nobody but
    /// alice and her agent trades her account; at a price with no spread a
    /// trade leaves what it is worth where it was.
    function testFuzz_venue_accounts(
        uint8 pick,
        address random,
        uint8 which,
        uint256 amount,
        address agent,
        bool buy
    ) public {
        address caller = pickCaller(pick, random);
        which %= 5;
        amount = bound(amount, 0, 20_000e6);
        VenueAccount memory before = accountOfAlice();
        int256 callerCash = venue.cashOf(caller);
        bool permitted = caller == alice || venue.isAgent(alice, caller);
        (bool ok, bytes4 err) = callAs(caller, address(venue), venueCall(which, amount, agent, buy));
        if (which == 4 && !permitted) {
            expectOutcome(ok, err, ILocalVenueErrors.NotPermitted.selector, "place, by a stranger");
        }
        if (which == 3 && callerCash < int256(amount)) {
            expectOutcome(ok, err, ILocalVenueErrors.WouldGoNegative.selector, "withdraw, beyond the account");
        }
        if (which < 2) expectOutcome(ok, err, bytes4(0), "an agent change on one's own account");
        if (caller == alice) return;
        VenueAccount memory afterwards = accountOfAlice();
        assertEq(afterwards.usd, before.usd, "someone else moved alice's dollars");
        assertEq(afterwards.worth, before.worth, "someone else changed what alice's account is worth");
        if (which != 4) assertEq(afterwards.cash, before.cash, "someone else moved alice's venue cash");
        assertEq(afterwards.bobAgent, before.bobAgent, "someone else changed alice's agents");
    }
}
