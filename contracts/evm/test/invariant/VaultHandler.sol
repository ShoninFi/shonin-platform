// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../../src/AttestationRegistry.sol";
import {DevAttestationVerifier} from "../../src/DevAttestationVerifier.sol";
import {LocalDriver} from "../../src/LocalDriver.sol";
import {LocalUsd} from "../../src/LocalUsd.sol";
import {LocalVenue} from "../../src/LocalVenue.sol";
import {Prices} from "../../src/Prices.sol";
import {Station} from "../../src/Station.sol";
import {Vault} from "../../src/Vault.sol";
import {VaultFactory} from "../../src/VaultFactory.sol";
import {ImageKind} from "../../src/interfaces/IAttestationRegistry.sol";
import {Call, Custody, ExitPath, IBridge, IDriver, ValuationKind} from "../../src/interfaces/IDriver.sol";
import {ILocalVenue, Market, OrderRequest} from "../../src/interfaces/ILocalVenue.sol";
import {Allowances, UnitPrices} from "../../src/interfaces/IPrices.sol";
import {
    ClaimState,
    IVault,
    Request,
    RequestKind,
    Tank,
    Valuation,
    ValuationStatus,
    VaultState
} from "../../src/interfaces/IVault.sol";
import {VaultConfig} from "../../src/interfaces/IVaultFactory.sol";
import {Test} from "forge-std/Test.sol";
import {Vm} from "forge-std/Vm.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// A bridge the admin wrote: whatever a station gives it to send goes to the
/// admin instead.
contract KeepingBridge is IBridge {
    IERC20 public immutable asset;
    address public immutable keeper;

    constructor(IERC20 asset_, address keeper_) {
        asset = asset_;
        keeper = keeper_;
    }

    function name() external pure returns (bytes32) {
        return "keeping-bridge";
    }

    function targets() public view returns (address[] memory list) {
        list = new address[](1);
        list[0] = address(this);
    }

    function send(
        address,
        uint256,
        address,
        uint256 amount,
        bytes calldata
    ) external view returns (Call[] memory calls) {
        calls = new Call[](2);
        calls[0] = Call(address(asset), 0, abi.encodeCall(IERC20.approve, (address(this), amount)));
        calls[1] = Call(address(this), 0, abi.encodeCall(this.take, (amount)));
    }

    /// Take from whoever approved it.
    function take(uint256 amount) external {
        asset.transferFrom(msg.sender, keeper, amount);
    }
}

/// A driver the admin wrote: every call it prepares hands the station's
/// dollars to the admin.
contract TakingDriver is IDriver {
    IERC20 public immutable asset;
    address public immutable keeper;

    constructor(IERC20 asset_, address keeper_) {
        asset = asset_;
        keeper = keeper_;
    }

    function name() external pure returns (bytes32) {
        return "taking";
    }

    function targets() public view returns (address[] memory list) {
        list = new address[](1);
        list[0] = address(this);
    }

    function restricted() external pure returns (bytes32[] memory) {
        return new bytes32[](0);
    }

    function custody() external pure returns (Custody) {
        return Custody.Venue;
    }

    function exitPath() external pure returns (ExitPath) {
        return ExitPath.Contract;
    }

    function valuation() external pure returns (ValuationKind) {
        return ValuationKind.OnChain;
    }

    function approveKey(address station, address, uint64) external view returns (Call[] memory) {
        return _takeAll(station);
    }

    function revokeKey(address station, address) external view returns (Call[] memory) {
        return _takeAll(station);
    }

    function deposit(address station, uint256) external view returns (Call[] memory) {
        return _takeAll(station);
    }

    function withdraw(address station, uint256) external view returns (Call[] memory) {
        return _takeAll(station);
    }

    function close(address station, uint256, uint16) external view returns (Call[] memory, bool) {
        return (_takeAll(station), false);
    }

    function sweep(address station, uint256) external view returns (Call[] memory) {
        return _takeAll(station);
    }

    function valueOf(address) external pure returns (uint256, uint256) {
        return (0, 0);
    }

    function take(address from) external {
        asset.transferFrom(from, keeper, asset.balanceOf(from));
    }

    function _takeAll(address station) private view returns (Call[] memory calls) {
        calls = new Call[](2);
        calls[0] = Call(address(asset), 0, abi.encodeCall(IERC20.approve, (address(this), type(uint256).max)));
        calls[1] = Call(address(this), 0, abi.encodeCall(this.take, (station)));
    }
}

/// What the handler drives: one single-chain vault and the platform around it.
struct Deps {
    Vault vault;
    LocalUsd usd;
    LocalVenue venue;
    AttestationRegistry attestation;
    Prices prices;
    VaultFactory factory;
    LocalDriver driver;
    address admin;
    address platform;
    address operator;
    address author;
    uint256 authorKey;
    uint256 executorKey;
    uint256 attestorKey;
    bytes32 executorImage;
    bytes32 project;
}

/// Drives a single-chain vault through everything anyone can do to it, with
/// bounded random inputs, and keeps the ghost books the invariants read.
///
/// Every action measures the vault before and after itself and reads the
/// events it caused, so the properties that are about one step (a passive
/// holder's price, what a payday mints, where money went, what the admin
/// reached) are judged step by step and recorded as breaks; the invariants
/// then require that there were none. A few actions are thought experiments:
/// they run a scenario on a copy of the chain, read the outcome and throw the
/// copy away, so the walk itself carries on undisturbed.
contract VaultHandler is Test {
    using Math for uint256;

    uint256 internal constant OFFSET = 1e12;
    uint256 internal constant WAD = 1e18;
    uint256 internal constant BPS = 10_000;
    uint256 internal constant YEAR = 365 days;
    uint64 internal constant SLOT = 10;
    uint256 internal constant START_PRICE = 60_000e6;
    address internal constant DEAD = 0x000000000000000000000000000000000000dEaD;
    bytes32 internal constant TRANSFER = keccak256("Transfer(address,address,uint256)");
    bytes32 internal constant SPOT = "BTC";
    bytes32 internal constant PERP = "BTC-PERP";

    Vault public vault;
    Station public station;
    LocalUsd public usd;
    LocalVenue public venue;
    AttestationRegistry internal attestation;
    Prices internal prices;
    VaultFactory internal factory;
    LocalDriver internal driver;
    bytes32 internal bundle;
    bytes32 internal executorImage;
    bytes32 internal project;

    address public admin;
    address public platform;
    address internal operator;
    address public author;
    address public executor;
    uint256 internal executorKey;
    uint256 internal attestorKey;
    /// Calls what anyone may call.
    address public keeper = makeAddr("keeper");
    /// Sends money in from outside.
    address public stranger = makeAddr("stranger");
    /// Deposits once and never acts again: the holder whose price is watched.
    address public passive;
    /// The thought experiments' newcomer.
    address internal newcomer;
    address[] internal actors;
    mapping(address => uint256) internal keyOf;
    /// Who trades for the station: the executor, or a key the admin holds.
    address internal trader;

    // What the admin can bring in over time ------------------------------------

    /// Keys the admin holds: one from an executor image of its own, one from
    /// an unwinder image of its own, and one passed off as the platform's
    /// executor by a checker of its own.
    address public adminKey;
    address public adminUnwinder;
    address public adminImposter;
    /// The key behind the admin's own attestation checker.
    uint256 internal adminAttestorKey;
    DevAttestationVerifier public adminVerifier;
    bytes32 internal constant ADMIN_IMAGE = keccak256("the admin's executor image");
    bytes32 internal constant ADMIN_UNWINDER_IMAGE = keccak256("the admin's unwinder image");
    bytes32 internal constant ADMIN_PROJECT = keccak256("the admin's project");
    /// A working driver the admin deploys and puts in under the vault's
    /// driver name, a taking one, and a bridge that keeps what it is given.
    LocalDriver internal adminDriver;
    TakingDriver internal takingDriver;
    KeepingBridge internal keepingBridge;
    /// Every address the admin controls: none may ever receive a dollar.
    address[] public adminAddresses;
    mapping(address => bool) internal adminOwns;

    /// The venue's price, which every move starts from.
    uint256 public price = START_PRICE;

    // Ghost books -------------------------------------------------------------

    /// No market move since the last priced valuation: settling meets the
    /// pool exactly as that valuation's snapshot found it.
    bool public still;
    /// Everyone who has ever held a share, from the vault's own transfers.
    address[] public holders;
    mapping(address => bool) internal held;
    /// Who the vault may pay: everyone who asked to deposit or held a share,
    /// the author and the platform.
    mapping(address => bool) public entitled;
    address[] public payees;
    /// Holding-fee shares minted and not yet handed over, and when the
    /// holding fee was last synced.
    uint256 public unpaidHolding;
    uint64 internal lastHolding;
    /// The tank, from its own events: what it started with, what came in,
    /// what was charged and what the author took.
    uint256 public tankStart;
    uint256 public tankIn;
    uint256 public tankCharged;
    uint256 public tankOut;
    uint256 internal lastUsage;
    uint64 internal lastCollection;
    uint64 internal billAsOf;
    /// The allowances in force and when each was set, so the cap can be
    /// priced independently of the vault.
    uint64[] internal allowanceSince;
    Allowances[] internal allowanceAt;
    /// The idle floor as the author set it: a lowering waits a day.
    uint16 internal floorBps;
    uint16 internal pendingFloorBps;
    uint64 internal pendingFloorAt;
    /// The shares' part of the station at the last priced valuation, as the
    /// handler read the station itself just before the vault did, and the
    /// dollars that went from the vault to the station and back since, as
    /// the dollar's own transfers tell it.
    uint256 internal stationsAtValuation;
    uint256 internal pulledSince;
    uint256 internal pushedSince;
    uint256 internal lastPricedIndex;
    uint256 internal lastMark;
    /// Dollars that left the vault, its station or the venue in this step.
    uint256 internal outflows;

    /// The pool as the handler measured it just before a valuation: what the
    /// vault must record.
    struct Measured {
        bool set;
        uint256 gross;
        uint256 grossExit;
        uint256 stations;
    }

    Measured internal measured;

    /// Every request as it was asked for: who asked, what for and how much,
    /// from the handler's own calls, never from the vault's records. A claim's
    /// owner and shares as it was booked, and the shares each holder has
    /// asked to redeem and not yet been paid, booked or sent to the fallback
    /// for.
    mapping(uint256 => address) internal askedBy;
    mapping(uint256 => uint256) internal askedFor;
    mapping(uint256 => RequestKind) internal askedKind;
    mapping(uint256 => uint256) internal claimOfRequest;
    mapping(uint256 => address) internal claimOwner;
    mapping(uint256 => uint256) internal claimShares;
    mapping(address => uint256) internal redeeming;

    /// One payment out of the vault: to whom and how much.
    struct Payment {
        address to;
        uint256 amount;
    }

    /// The payments this step's events say the vault made, and the ones the
    /// step itself expects that no event announces (collecting a held
    /// payout).
    Payment[] internal owedOut;
    /// Who the step pays and what for, when the vault's event names nobody
    /// the handler can check against a request.
    address internal stepPayee;
    uint256 internal stepRequest;
    uint256 internal stepClaimable;

    // Breaks, by invariant ------------------------------------------------------

    uint256 public payoutBreaks;
    uint256 public passiveBreaks;
    uint256 public roundTripBreaks;
    uint256 public feeBreaks;
    uint256 public markBreaks;
    uint256 public givenBackBreaks;
    uint256 public tankBreaks;
    uint256 public moneyBreaks;
    uint256 public floorBreaks;
    uint256 public adminBreaks;
    uint256 public graceBreaks;
    string public lastBreak;

    /// How often each action got through, for tuning the walk; names that
    /// start with `~` count the checks made and the events seen.
    mapping(bytes32 => uint256) public hits;
    bytes32[] public hitNames;

    // The step being taken ------------------------------------------------------

    struct Before {
        uint256 price;
        uint256 supply;
        uint256 mark;
        uint256 vaultUsd;
        uint256 stationUsd;
        int256 venueCash;
        uint256 cash;
        uint256 escrow;
        uint256 tank;
        uint256 adminUsd;
        uint256 reserve;
        VaultState state;
    }

    Before internal before;

    constructor(Deps memory d) {
        vault = d.vault;
        usd = d.usd;
        venue = d.venue;
        attestation = d.attestation;
        prices = d.prices;
        factory = d.factory;
        driver = d.driver;
        admin = d.admin;
        platform = d.platform;
        operator = d.operator;
        author = d.author;
        executorKey = d.executorKey;
        attestorKey = d.attestorKey;
        executorImage = d.executorImage;
        project = d.project;
        executor = vm.addr(d.executorKey);
        station = Station(vault.homeStation());
        bundle = vault.bundleHash();

        string[4] memory names = ["alice", "bob", "carol", "erin"];
        for (uint256 i = 0; i < names.length; i++) {
            (address who, uint256 key) = makeAddrAndKey(names[i]);
            actors.push(who);
            keyOf[who] = key;
        }
        actors.push(author);
        keyOf[author] = d.authorKey;
        uint256 passiveKey;
        (passive, passiveKey) = makeAddrAndKey("passive");
        keyOf[passive] = passiveKey;
        uint256 newcomerKey;
        (newcomer, newcomerKey) = makeAddrAndKey("newcomer");
        keyOf[newcomer] = newcomerKey;
        trader = executor;

        uint256 secret;
        (adminKey, secret) = makeAddrAndKey("the admin's executor key");
        keyOf[adminKey] = secret;
        (adminUnwinder, secret) = makeAddrAndKey("the admin's unwinder key");
        keyOf[adminUnwinder] = secret;
        (adminImposter, secret) = makeAddrAndKey("the admin's imposter key");
        keyOf[adminImposter] = secret;
        address adminAttestor;
        (adminAttestor, adminAttestorKey) = makeAddrAndKey("the admin's attestor");
        adminVerifier = new DevAttestationVerifier(adminAttestor);
        adminDriver = new LocalDriver(venue, IERC20(address(usd)));
        takingDriver = new TakingDriver(IERC20(address(usd)), admin);
        keepingBridge = new KeepingBridge(IERC20(address(usd)), admin);
        address[8] memory owned = [
            admin,
            adminKey,
            adminUnwinder,
            adminImposter,
            adminAttestor,
            address(adminVerifier),
            address(takingDriver),
            address(keepingBridge)
        ];
        for (uint256 i = 0; i < owned.length; i++) {
            adminAddresses.push(owned[i]);
            adminOwns[owned[i]] = true;
        }

        _entitle(author);
        _entitle(platform);
        lastHolding = vault.createdAt();
        lastCollection = vault.createdAt();
        allowanceSince.push(vault.createdAt());
        allowanceAt.push(vault.allowances());
        (floorBps,,) = vault.idleFloor();
        tankStart = vault.tank().balance;
        lastMark = vault.mark();
        _noteHolder(DEAD);
    }

    /// The passive holder and every actor join, the executor takes the
    /// venue, and the vault is valued once.
    function bootstrap() external {
        _begin();
        usd.mint(address(venue), 100_000_000e6);
        _deposit(passive, 100_000e6);
        for (uint256 i = 0; i < actors.length; i++) {
            _deposit(actors[i], 20_000e6 * (i + 1));
        }
        vm.prank(executor);
        station.approveKey("local", executor);
        vm.warp(_nextSlot());
        _record();
        vault.settle(type(uint256).max);
        _end(true, "bootstrap");
    }

    function requestRedeem(uint256 who, uint256 pct) public {
        _requestRedeem(who, pct);
    }

    function warp(uint256 kind, uint256 amount) public {
        _warp(kind, amount);
    }

    // Grouped actions ------------------------------------------------------------

    /// The executor: pull, push, the venue account, orders, and unwinding.
    function executorAct(uint256 op, uint256 a, uint256 b) public {
        uint256 k = op % 12;
        if (k <= 2) {
            // Put cash to work: pull it, move it to the venue and buy.
            _pull(a);
            _venueDeposit(100);
            _trade(b % 2 == 0, true, b >> 8, false);
        } else if (k == 3) {
            // Bring some home: sell, withdraw from the venue and push.
            _trade(b % 2 == 0, false, b >> 8, true);
            _venueWithdraw(a);
            _push(100);
        } else if (k == 4) {
            _pull(a);
        } else if (k == 5) {
            _push(a);
        } else if (k == 6) {
            _venueDeposit(a);
        } else if (k == 7) {
            _venueWithdraw(a);
        } else if (k <= 10) {
            _trade(b % 2 == 0, (b >> 1) % 2 == 0, a, (b >> 2) % 4 == 0);
        } else {
            _executorUnwind(a);
        }
    }

    /// Claims, the fallbacks, freezing and units.
    function claimAct(uint256 op, uint256 seed, bool flag) public {
        uint256 k = op % 14;
        if (k <= 2) {
            _settleClaim(seed);
        } else if (k == 3) {
            _claimInstead(seed);
        } else if (k <= 5) {
            _takeProRata(seed);
        } else if (k <= 7) {
            _takeProRataClaim(seed);
        } else if (k == 8) {
            _freeze(seed);
        } else if (k <= 10) {
            _serveProRata(seed, flag);
        } else if (k == 11) {
            _collectUnits(seed);
        } else if (k == 12) {
            // Nobody values for a day: a leaver takes the fallback, or
            // books a claim instead.
            _requestRedeem(seed, seed >> 8);
            _warp(1, 0);
            if (flag) _takeProRata(vault.requestCount());
            else _claimInstead(vault.requestCount());
        } else {
            // A claim nobody could serve for a day takes the fallback.
            _warp(1, 0);
            _takeProRataClaim(seed);
        }
    }

    /// The tank, the allowances and the idle floor.
    function tankAct(uint256 op, uint256 a, uint256 b, bool flag) public {
        uint256 k = op % 9;
        if (k <= 1) _keep(a, flag);
        else if (k == 2) _setTankKeep(a);
        else if (k == 3) _topUpFromShares(a);
        else if (k == 4) _fundTank(a);
        else if (k == 5) _withdrawTank(a, b);
        else if (k == 6) _withdrawTankUnsigned(a, flag);
        else if (k == 7) _setAllowances(a, b);
        else _setIdleFloor(a);
    }

    /// Retiring, evicting, killing, stopping, unwinding and closing.
    function lifecycleAct(uint256 op, uint256 seed) public {
        uint256 k = op % 7;
        if (k == 0) _retire(seed);
        else if (k == 1) _evict();
        else if (k == 2) _kill(seed);
        else if (k == 3) _stopStation();
        else if (k == 4) _unwindStation();
        else if (k == 5) _windDown(seed);
        else _close();
    }

    /// A closed vault's doors, and payouts held for a refusing receiver.
    function closedAct(uint256 op, uint256 seed, uint256 pct) public {
        uint256 k = op % 4;
        if (k == 0) _redeemClosed(seed, pct);
        else if (k == 1) _settleClosed(seed);
        else if (k == 2) _claimClosed(seed);
        else _collectHeld(seed);
    }

    /// A receiver who refuses their payout, money sent in from outside,
    /// and collecting what was held.
    function miscAct(uint256 op, uint256 a, uint256 b, bool flag) public {
        uint256 k = op % 4;
        if (k <= 1) _settleRefusing(a, b, flag);
        else if (k == 2) _donate(b, flag);
        else _collectHeld(a);
    }

    /// The thought experiments.
    function experimentAct(uint256 op, uint256 a, bool flag) public {
        uint256 k = op % 3;
        if (k == 0) _roundTrip(a);
        else if (k == 1) _gainGivenBack(a, flag);
        else _payEveryoneOut();
    }

    // Depositors ----------------------------------------------------------------

    /// Ask to deposit, signing the statement or not, or for a receiver who
    /// never signed (which must be refused).
    function requestDeposit(uint256 who, uint256 amount, uint8 mode) public {
        address a = _actor(who);
        amount = _bound(amount, 1e6, 300_000e6);
        _begin();
        address receiver = mode % 16 == 15 ? stranger : a;
        bytes memory statement = mode % 3 == 0 || receiver == stranger ? bytes("") : _statement(a);
        usd.mint(a, amount);
        vm.startPrank(a);
        usd.approve(address(vault), amount);
        try vault.requestDeposit(amount, receiver, statement) returns (uint256 id) {
            _entitle(a);
            _noteAsk(id, receiver, amount, RequestKind.Deposit);
            _hit("requestDeposit");
        } catch {}
        vm.stopPrank();
        _end(true, "requestDeposit");
    }

    function _requestRedeem(uint256 who, uint256 pct) internal {
        address a = _actor(who);
        uint256 shares = vault.balanceOf(a) * _bound(pct, 1, 100) / 100;
        if (shares == 0) return;
        _begin();
        vm.prank(a);
        try vault.requestRedeem(shares, a) returns (uint256 id) {
            _noteAsk(id, a, shares, RequestKind.Redeem);
            redeeming[a] += shares;
            _hit("requestRedeem");
        } catch {}
        _end(true, "requestRedeem");
    }

    // Valuations and settling ---------------------------------------------------

    /// Move to the next slot boundary, sometimes past its window, and value.
    function recordValuation(uint256 late) public {
        _begin();
        bool calm = still;
        // Usually whoever values settles what is owed first.
        if (late % 3 != 0) try vault.settle(type(uint256).max) {} catch {}
        // Mostly inside the slot's window, sometimes just past it.
        vm.warp(_nextSlot() + (late % 10 < 8 ? late % 4 : 4));
        if (_record()) _hit("recordValuation");
        _end(calm || late % 3 == 0, "recordValuation");
    }

    function settle(uint256 max) public {
        max = max % 4 == 0 ? type(uint256).max : _bound(max, 1, 4);
        _begin();
        bool calm = still;
        try vault.settle(max) {
            _hit("settle");
        } catch {
            _break("settle reverted");
            moneyBreaks++;
        }
        _end(calm, "settle");
    }

    /// A receiver whose dollar refuses them: what they are owed is held for
    /// them and the queue moves on.
    function _settleRefusing(uint256 who, uint256 claimSeed, bool claim) internal {
        address a = _actor(who);
        _begin();
        bool calm = still;
        vm.mockCallRevert(address(usd), abi.encodeWithSelector(usd.transfer.selector, a), "refused");
        if (claim) {
            uint256 id = _openClaim(claimSeed);
            if (id != 0) {
                vm.prank(keeper);
                try vault.settleClaim(id) {
                    _hit("settleClaimRefusing");
                } catch {
                    _dropReverted();
                }
            }
        } else {
            try vault.settle(type(uint256).max) {
                _hit("settleRefusing");
            } catch {
                _break("settle reverted on a refused payout");
                moneyBreaks++;
            }
        }
        vm.clearMockedCalls();
        _end(claim || calm, "settleRefusing");
    }

    function _collectHeld(uint256 who) internal {
        address a = who % 3 == 0 ? platform : _actor(who);
        uint256 held_ = vault.heldFor(a);
        if (held_ == 0) return;
        _begin();
        vm.prank(a);
        try vault.collectHeld() {
            // No event announces it: what was held for them, to them.
            _expect(a, held_);
            _hit("collectHeld");
        } catch {}
        _end(true, "collectHeld");
    }

    // Time ---------------------------------------------------------------------

    /// A few seconds to a few days, sometimes straight across a Monday. The
    /// executor renews its blessing and its venue approval.
    function _warp(uint256 kind, uint256 amount) internal {
        _begin();
        uint256 t = block.timestamp;
        uint256 k = kind % 5;
        if (k == 0) {
            vm.warp(t + _bound(amount, 1, 1 hours));
        } else if (k == 1) {
            vm.warp(t + 1 days + SLOT);
        } else if (k == 2) {
            vm.warp(_nextMonday(t) + _bound(amount, 0, 2 hours));
        } else if (k == 3) {
            vm.warp(t + 3 days);
        } else {
            vm.warp(t + 10 minutes + 1);
        }
        _renew();
        _hit("warp");
        _end(true, "warp");
    }

    // The market ---------------------------------------------------------------

    /// Move both markets: inside the exit band, with the bid outside it,
    /// with the venue's mark off its own price, or with the bid above the
    /// mark.
    function movePrice(uint256 moveBps, bool up, uint256 spreadBps, uint8 mode) public {
        _begin();
        moveBps = _bound(moveBps, 0, 800);
        uint256 p = up ? price * (BPS + moveBps) / BPS : price * (BPS - moveBps) / BPS;
        if (p < START_PRICE / 2) p = START_PRICE / 2;
        if (p > START_PRICE * 2) p = START_PRICE * 2;
        price = p;
        uint256 m = mode % 8;
        uint256 mark = p;
        uint256 bid;
        uint256 ask;
        if (m <= 4) {
            // Inside the 1% band: half the spread is at most 0.9%.
            uint256 half = p * _bound(spreadBps, 0, 180) / 20_000;
            (bid, ask) = (p - half, p + half);
        } else if (m == 5) {
            uint256 half = p * _bound(spreadBps, 220, 800) / 20_000;
            (bid, ask) = (p - half, p + half);
        } else if (m == 6) {
            mark = p * (BPS + _bound(spreadBps, 120, 400)) / BPS;
            (bid, ask) = (p, p);
        } else {
            bid = p * (BPS + _bound(spreadBps, 1, 80)) / BPS;
            ask = bid + p / 1000;
        }
        vm.startPrank(operator);
        venue.setPrices(SPOT, mark, p, bid, ask);
        venue.setPrices(PERP, mark, p, bid, ask);
        vm.stopPrank();
        still = false;
        _hit("movePrice");
        _end(false, "movePrice");
    }

    // The executor ---------------------------------------------------------------

    function _pull(uint256 pct) internal {
        _begin();
        bool calm = still;
        // The executor settles what is owed first: nothing leaves while a
        // priced valuation waits.
        try vault.settle(type(uint256).max) {} catch {}
        uint256 amount = vault.cash() * _bound(pct, 1, 100) / 100;
        uint256 floor = _floorFor(vault.cash());
        vm.prank(trader);
        try station.pull(amount) {
            _hit(trader == executor ? bytes32("pull") : bytes32("pull by an admin's key"));
            if (floor != 0 && vault.cash() < floor + floor / 50) _hit("~pull near the floor");
            // The home station can't pull cash below the idle floor.
            if (vault.cash() < floor) {
                floorBreaks++;
                _break(
                    string.concat(
                        "a pull left cash of ",
                        vm.toString(vault.cash()),
                        " under a floor of ",
                        vm.toString(floor)
                    )
                );
            }
        } catch {}
        _end(calm, "pull");
    }

    /// The idle floor in force, measured on the pool's value as the handler
    /// reads it, not as the vault reckons it: the cash now, the shares' part
    /// of the station as the handler read it at the last valuation, and the
    /// dollars that went out to the station since less those that came back.
    /// Money sent home is taken off even when it was never counted at the
    /// station (a donation, say), so this is never above the value the vault
    /// should use, and a pull that leaves the cash under it took too much.
    function _floorFor(uint256 cash) internal view returns (uint256) {
        uint256 value = cash + stationsAtValuation + pulledSince + 1;
        value = value > pushedSince ? value - pushedSince : 0;
        return value.mulDiv(_floorNow(), BPS, Math.Rounding.Ceil);
    }

    function _push(uint256 pct) internal {
        uint256 amount = usd.balanceOf(address(station)) * _bound(pct, 1, 100) / 100;
        if (amount == 0) return;
        _begin();
        vm.prank(trader);
        try station.push(amount) {
            _hit(trader == executor ? bytes32("push") : bytes32("push by an admin's key"));
        } catch {}
        _end(true, "push");
    }

    function _venueDeposit(uint256 pct) internal {
        uint256 amount = usd.balanceOf(address(station)) * _bound(pct, 1, 100) / 100;
        if (amount == 0) return;
        // Money put into an account the market has left underwater fills the
        // hole: that is the strategy's loss, not anyone's move.
        bool solvent = _venueSolvent();
        _begin();
        vm.prank(trader);
        try station.deposit("local", amount) {
            _hit(trader == executor ? bytes32("venueDeposit") : bytes32("venueDeposit by an admin's key"));
        } catch {}
        _end(solvent, "venueDeposit");
    }

    function _venueWithdraw(uint256 pct) internal {
        int256 cash = venue.cashOf(address(station));
        if (cash <= 0) return;
        uint256 amount = uint256(cash) * _bound(pct, 1, 100) / 100;
        if (amount == 0) return;
        _begin();
        vm.prank(trader);
        try station.withdraw("local", amount) {
            _hit(trader == executor ? bytes32("venueWithdraw") : bytes32("venueWithdraw by an admin's key"));
        } catch {}
        _end(true, "venueWithdraw");
    }

    /// A spot or perp order of up to the account's equity (spot) or three
    /// times it (perp), at whatever the market gives.
    function _trade(bool perp, bool isBuy, uint256 pct, bool reduceOnly) internal {
        uint256 equity = venue.accountValue(address(station));
        if (equity == 0) return;
        bytes32 market = perp ? PERP : SPOT;
        uint256 notional = equity * _bound(pct, 1, perp ? 300 : 100) / 100;
        uint256 size = notional * 1e18 / price;
        if (size == 0) return;
        _begin();
        OrderRequest[] memory orders = new OrderRequest[](1);
        orders[0] = OrderRequest({
            market: market,
            isBuy: isBuy,
            size: size,
            limitPrice: isBuy ? type(uint256).max : 0,
            reduceOnly: reduceOnly
        });
        vm.prank(trader);
        try venue.place(address(station), orders) {
            still = false;
            _hit(trader == executor ? bytes32("trade") : bytes32("trade by an admin's key"));
        } catch {}
        _end(false, "trade");
    }

    /// The executor closes a fraction of everything and sends it home.
    function _executorUnwind(uint256 fractionBps) internal {
        uint256 fraction = _bound(fractionBps, 1, BPS) * WAD / BPS;
        _begin();
        vm.prank(trader);
        try station.unwind(fraction) {
            still = false;
            _hit(trader == executor ? bytes32("executorUnwind") : bytes32("unwind by an admin's key"));
        } catch {}
        _end(false, "executorUnwind");
    }

    // Claims and the fallback --------------------------------------------------

    function _settleClaim(uint256 seed) internal {
        uint256 id = _openClaim(seed);
        if (id == 0) return;
        _begin();
        vm.prank(keeper);
        try vault.settleClaim(id) {
            _hit("settleClaim");
        } catch {
            _dropReverted();
        }
        _end(true, "settleClaim");
    }

    function _claimInstead(uint256 seed) internal {
        uint256 id = _openRequest(seed);
        if (id == 0) return;
        _begin();
        stepRequest = id;
        vm.prank(vault.requestAt(id).owner);
        try vault.claimInstead(id) {
            _hit("claimInstead");
        } catch {
            _dropReverted();
        }
        _end(true, "claimInstead");
    }

    function _takeProRata(uint256 seed) internal {
        uint256 id = _openRequest(seed);
        if (id == 0) return;
        _begin();
        (stepPayee, stepRequest) = (askedBy[id], id);
        vm.prank(vault.requestAt(id).owner);
        try vault.takeProRata(id) {
            _hit("takeProRata");
        } catch {
            _dropReverted();
        }
        _end(true, "takeProRata");
    }

    function _takeProRataClaim(uint256 seed) internal {
        uint256 id = _openClaim(seed);
        if (id == 0) return;
        _begin();
        stepPayee = claimOwner[id];
        vm.prank(vault.claimAt(id).owner);
        try vault.takeProRataClaim(id) {
            _hit("takeProRataClaim");
        } catch {
            _dropReverted();
        }
        _end(true, "takeProRataClaim");
    }

    function _freeze(uint256 seed) internal {
        uint256 id = _openClaim(seed);
        if (id == 0) return;
        _begin();
        vm.prank(keeper);
        try vault.freeze(id) {
            _hit("freeze");
        } catch {
            _dropReverted();
        }
        _end(true, "freeze");
    }

    /// Serve a unit holder, asked by them or by anyone.
    function _serveProRata(uint256 who, bool byHolder) internal {
        address a = _unitHolder(who);
        if (a == address(0)) return;
        _begin();
        vm.prank(byHolder ? a : keeper);
        try vault.serveProRata(a) {
            _hit("serveProRata");
        } catch {
            _dropReverted();
        }
        _end(true, "serveProRata");
    }

    function _collectUnits(uint256 who) internal {
        address a = _unitHolder(who);
        if (a == address(0)) return;
        _begin();
        stepPayee = a;
        (, stepClaimable) = vault.unitsOf(a);
        vm.prank(a);
        try vault.collectUnits() {
            _hit("collectUnits");
        } catch {
            _dropReverted();
        }
        _end(true, "collectUnits");
    }

    // The tank -----------------------------------------------------------------

    /// Keep the vault, with the executor's signed running total or without.
    function _keep(uint256 more, bool signed) internal {
        _begin();
        Tank memory t = vault.tank();
        uint64 asOf = uint64(block.timestamp);
        uint256 total = t.usageSettled + _bound(more, 0, 30e6);
        bytes memory signature;
        if (signed && asOf > t.usageAsOf) {
            signature = _usage(total, asOf);
            billAsOf = asOf;
        }
        vm.prank(keeper);
        try vault.keep(total, asOf, signature) {
            _hit(signed ? bytes32("keepSigned") : bytes32("keep"));
        } catch {}
        _end(true, "keep");
    }

    function _setTankKeep(uint256 amount) internal {
        _begin();
        vm.prank(author);
        try vault.setTankKeep(_bound(amount, 0, 200e6)) {
            _hit("setTankKeep");
        } catch {}
        _end(true, "setTankKeep");
    }

    function _topUpFromShares(uint256 pct) internal {
        uint256 shares = vault.balanceOf(author) * _bound(pct, 1, 50) / 100;
        if (shares == 0) return;
        _begin();
        vm.prank(author);
        try vault.topUpFromShares(shares) {
            _hit("topUpFromShares");
        } catch {}
        _end(true, "topUpFromShares");
    }

    function _fundTank(uint256 amount) internal {
        amount = _bound(amount, 1, 100e6);
        _begin();
        usd.mint(stranger, amount);
        vm.startPrank(stranger);
        usd.approve(address(vault), amount);
        try vault.fundTank(amount) {
            _hit("fundTank");
        } catch {}
        vm.stopPrank();
        _end(true, "fundTank");
    }

    /// The author withdraws against a fresh signed total.
    function _withdrawTank(uint256 amount, uint256 more) internal {
        _begin();
        Tank memory t = vault.tank();
        uint64 asOf = uint64(block.timestamp);
        uint256 total = t.usageSettled + _bound(more, 0, 30e6);
        bytes memory signature;
        if (asOf > t.usageAsOf) {
            signature = _usage(total, asOf);
            billAsOf = asOf;
        } else {
            (total, asOf) = (t.usageSettled, t.usageAsOf);
        }
        vm.prank(author);
        try vault.withdrawTank(_bound(amount, 0, 100e6), total, asOf, signature) {
            _hit("withdrawTank");
        } catch {}
        _end(true, "withdrawTank");
    }

    /// The escape for a vault with no executor running: ask, wait a day,
    /// withdraw unsigned.
    function _withdrawTankUnsigned(uint256 amount, bool ask) internal {
        _begin();
        vm.startPrank(author);
        if (ask) {
            try vault.requestTankWithdrawal() {
                _hit("requestTankWithdrawal");
            } catch {}
        } else {
            try vault.withdrawTankUnsigned(_bound(amount, 0, 100e6)) {
                _hit("withdrawTankUnsigned");
            } catch {}
        }
        vm.stopPrank();
        _end(true, "withdrawTankUnsigned");
    }

    function _setAllowances(uint256 perDay, uint256 fuel) internal {
        Allowances memory a;
        a.gasPerDay = uint64(_bound(perDay, 0, 10e6));
        a.fuelPerSec = uint64(_bound(fuel, 0, 2_000_000));
        _begin();
        vm.prank(author);
        try vault.setAllowances(a) {
            allowanceSince.push(uint64(block.timestamp));
            allowanceAt.push(a);
            _hit("setAllowances");
        } catch {}
        _end(true, "setAllowances");
    }

    function _setIdleFloor(uint256 bps) internal {
        uint16 b = uint16(_bound(bps, 0, 3000));
        _begin();
        uint16 current = _floorNow();
        vm.prank(author);
        try vault.setIdleFloor(b) {
            floorBps = current;
            if (b >= current) {
                (floorBps, pendingFloorBps, pendingFloorAt) = (b, 0, 0);
            } else {
                (pendingFloorBps, pendingFloorAt) = (b, uint64(block.timestamp) + 1 days);
            }
            _hit("setIdleFloor");
        } catch {}
        _end(true, "setIdleFloor");
    }

    // Lifecycle ------------------------------------------------------------------

    function _retire(uint256 seed) internal {
        if (seed % 6 != 0) return;
        _begin();
        vm.prank(author);
        try vault.retire() {
            _hit("retire");
        } catch {}
        _end(true, "retire");
    }

    function _evict() internal {
        _begin();
        vm.prank(keeper);
        try vault.evict() {
            _hit("evict");
        } catch {}
        _end(true, "evict");
    }

    function _kill(uint256 seed) internal {
        if (seed % 12 != 0) return;
        _begin();
        vm.prank(admin);
        try vault.kill() {
            _hit("kill");
        } catch {}
        _end(true, "kill");
    }

    /// Anyone may stop the home station, and only once the halt's grace has
    /// passed.
    function _stopStation() internal {
        _begin();
        vm.prank(keeper);
        try station.stop() {
            uint64 halted = vault.haltedAt();
            if (halted == 0 || block.timestamp < halted + vault.haltGrace()) {
                graceBreaks++;
                _break("the station stopped inside the halt's grace");
            }
            _hit("stopStation");
        } catch {}
        _end(true, "stopStation");
    }

    /// Anyone may unwind a stopped station, all of it.
    function _unwindStation() internal {
        _begin();
        vm.prank(keeper);
        try station.unwind(WAD) {
            still = false;
            _hit("unwindStation");
        } catch {}
        _end(false, "unwindStation");
    }

    /// The whole way down in one step: halt if it isn't, wait out the
    /// grace, stop, unwind, value, settle and close.
    function _windDown(uint256 seed) internal {
        if (seed % 10 != 0 || vault.state() == VaultState.Closed) return;
        _begin();
        if (vault.haltedAt() == 0) {
            vm.prank(admin);
            try vault.kill() {} catch {}
        }
        uint256 halted = vault.haltedAt();
        if (halted != 0) {
            uint256 t = halted + vault.haltGrace();
            if (t <= block.timestamp) t = block.timestamp + 1;
            vm.warp(t.ceilDiv(SLOT) * SLOT);
            if (!station.stopped()) try station.stop() {} catch {}
            // A levered account can be worth nothing at mark and something
            // at the bid: unwind whenever either side shows value.
            (uint256 m, uint256 mx) = station.value();
            if (m != 0 || mx != 0) try station.unwind(WAD) {} catch {}
            _record();
            try vault.settle(type(uint256).max) {} catch {}
            still = false;
            vm.prank(keeper);
            try vault.close() {
                _hit("windDown");
            } catch {}
        }
        _end(false, "windDown");
    }

    function _close() internal {
        _begin();
        vm.prank(keeper);
        try vault.close() {
            _hit("close");
        } catch {}
        _end(false, "close");
    }

    function _redeemClosed(uint256 who, uint256 pct) internal {
        address a = who % 4 == 0 ? platform : _actor(who);
        uint256 shares = vault.balanceOf(a) * _bound(pct, 1, 100) / 100;
        if (shares == 0) return;
        _begin();
        vm.prank(a);
        try vault.redeemClosed(shares, a) {
            _hit("redeemClosed");
        } catch {}
        _end(true, "redeemClosed");
    }

    function _settleClosed(uint256 seed) internal {
        uint256 id = _openRequest(seed);
        if (id == 0) return;
        _begin();
        vm.prank(keeper);
        try vault.settleClosed(id) {
            _hit("settleClosed");
        } catch {}
        _end(true, "settleClosed");
    }

    function _claimClosed(uint256 seed) internal {
        uint256 id = _openClaim(seed);
        if (id == 0) return;
        _begin();
        vm.prank(keeper);
        try vault.claimClosed(id) {
            _hit("claimClosed");
        } catch {}
        _end(true, "claimClosed");
    }

    /// Money sent to the vault is ignored; money sent to the station is a
    /// gain for everyone.
    function _donate(uint256 amount, bool toStation) internal {
        amount = _bound(amount, 1, 50_000e6);
        _begin();
        usd.mint(stranger, amount);
        vm.prank(stranger);
        usd.transfer(toStation ? address(station) : address(vault), amount);
        if (toStation) still = false;
        _hit("donate");
        _end(!toStation, "donate");
    }

    // The admin key --------------------------------------------------------------

    /// Everything the admin key can call, and everything that moves money
    /// that it must not be able to call. Nothing it does may move a dollar.
    function adminAct(uint256 which, uint256 seed) public {
        _begin();
        uint256 w = which % 33;
        bool mustFail;
        bool ok;
        vm.startPrank(admin);
        if (w == 0) {
            (ok,) = address(attestation).call(
                abi.encodeCall(AttestationRegistry.setTimelock, (uint64(_bound(seed, 0, 30 days))))
            );
        } else if (w == 1) {
            (ok,) = address(attestation).call(
                abi.encodeCall(
                    AttestationRegistry.proposeImage, (keccak256(abi.encode(seed)), ImageKind(1 + seed % 4))
                )
            );
        } else if (w == 2) {
            (ok,) = address(attestation).call(
                // Now and then the executor's own image: for good, since a
                // revoked image can't be proposed again.
                abi.encodeCall(
                    AttestationRegistry.revokeImage,
                    (seed % 40 == 0 ? executorImage : keccak256(abi.encode(seed)))
                )
            );
        } else if (w == 3) {
            (ok,) = address(attestation).call(
                abi.encodeCall(AttestationRegistry.proposeProject, (keccak256(abi.encode("p", seed))))
            );
        } else if (w == 4) {
            (ok,) = address(attestation).call(
                abi.encodeCall(
                    AttestationRegistry.revokeProject,
                    (seed % 40 == 0 ? project : keccak256(abi.encode("p", seed)))
                )
            );
        } else if (w == 5) {
            (ok,) = address(attestation).call(
                abi.encodeCall(AttestationRegistry.proposeVerifier, (address(uint160(seed | 1))))
            );
        } else if (w == 6) {
            (ok,) = address(attestation).call(
                abi.encodeCall(AttestationRegistry.retireVerifier, (address(uint160(seed | 1))))
            );
        } else if (w == 7) {
            // A driver under another name, or this vault's own again.
            bytes32 name = seed % 3 == 0 ? bytes32("local") : bytes32("other");
            (ok,) = address(attestation).call(
                abi.encodeCall(AttestationRegistry.proposeDriver, (name, address(driver)))
            );
        } else if (w == 8) {
            bytes32 name = seed % 5 == 0 ? bytes32("local") : bytes32("other");
            (ok,) = address(attestation).call(abi.encodeCall(AttestationRegistry.revokeDriver, (name)));
        } else if (w == 9) {
            (ok,) = address(attestation).call(
                abi.encodeCall(
                    AttestationRegistry.proposeBridge, ("other-bridge", address(uint160(seed | 1)))
                )
            );
        } else if (w == 10) {
            (ok,) =
                address(attestation).call(abi.encodeCall(AttestationRegistry.revokeBridge, ("other-bridge")));
        } else if (w == 11) {
            UnitPrices memory p = UnitPrices({
                vcpuHour: uint128(_bound(seed, 0, 1e18)),
                gibHour: uint128(seed % 1e17),
                gbEgress: uint128(seed % 3e17),
                inferenceSecond: uint128(seed % 1e14)
            });
            (ok,) = address(prices).call(abi.encodeCall(Prices.schedule, (p)));
        } else if (w == 12) {
            VaultConfig memory c = factory.config();
            c.holdingBps = uint16(_bound(seed, 0, 500));
            c.authorBps = uint16(seed % 3000);
            c.transactionBps = uint16(seed % 100);
            (ok,) = address(factory).call(abi.encodeCall(VaultFactory.setConfig, (c)));
        } else if (w == 13) {
            if (seed % 16 == 0) (ok,) = address(vault).call(abi.encodeCall(Vault.kill, ()));
        } else if (w == 14) {
            if (seed % 16 == 0) (ok,) = address(vault).call(abi.encodeCall(Vault.windDownSealed, ()));
        } else {
            // Nothing below is the admin's to do.
            mustFail = true;
            ok = _adminReaches(w, seed);
        }
        vm.stopPrank();
        if (ok) _hit(mustFail ? bytes32("adminReached") : bytes32("admin"));
        if (mustFail && ok) {
            adminBreaks++;
            _break(string.concat("the admin key got through call ", vm.toString(w)));
        }
        _end(true, "admin");
        _nothingMoved(string.concat("an admin call moved money: ", vm.toString(w)));
    }

    /// No money moved in the step and no book changed.
    function _nothingMoved(string memory why) internal {
        (, uint256 reserve) = vault.units();
        if (
            outflows != 0 || usd.balanceOf(address(vault)) != before.vaultUsd
                || usd.balanceOf(address(station)) != before.stationUsd
                || venue.cashOf(address(station)) != before.venueCash || vault.cash() != before.cash
                || vault.escrow() != before.escrow || vault.tank().balance != before.tank
                || usd.balanceOf(admin) != before.adminUsd || reserve != before.reserve
        ) {
            adminBreaks++;
            _break(why);
        }
    }

    /// A call the admin key must be refused: the author's, the executor's,
    /// the station's own and the venue account's.
    function _adminReaches(uint256 w, uint256 seed) internal returns (bool ok) {
        Allowances memory none;
        uint256 amount = _bound(seed, 1, 1000e6);
        bytes memory call;
        address target = address(vault);
        if (w == 15) {
            call = abi.encodeCall(Vault.setIdleFloor, (0));
        } else if (w == 16) {
            call = abi.encodeCall(Vault.retire, ());
        } else if (w == 17) {
            call = abi.encodeCall(Vault.withdrawTankUnsigned, (amount));
        } else if (w == 18) {
            call = abi.encodeCall(Vault.requestTankWithdrawal, ());
        } else if (w == 19) {
            call = abi.encodeCall(Vault.withdrawTank, (amount, 0, 0, ""));
        } else if (w == 20) {
            call = abi.encodeCall(Vault.topUpFromShares, (amount));
        } else if (w == 21) {
            call = abi.encodeCall(Vault.setTankKeep, (amount));
        } else if (w == 22) {
            call = abi.encodeCall(Vault.setAllowances, (none));
        } else if (w == 23) {
            call = abi.encodeCall(Vault.toStation, (amount));
        } else if (w == 24) {
            call = abi.encodeCall(Vault.fromStation, (amount));
        } else if (w == 25) {
            (target, call) = (address(station), abi.encodeCall(Station.pull, (amount)));
        } else if (w == 26) {
            (target, call) = (address(station), abi.encodeCall(Station.push, (amount)));
        } else if (w == 27) {
            (target, call) = (address(station), abi.encodeCall(Station.unwind, (WAD / 2)));
        } else if (w == 28) {
            (target, call) = (address(station), abi.encodeCall(Station.approveKey, ("local", admin)));
        } else if (w == 29) {
            (target, call) = (address(station), abi.encodeCall(Station.deposit, ("local", amount)));
        } else if (w == 30) {
            (target, call) = (address(station), abi.encodeCall(Station.withdraw, ("local", amount)));
        } else if (w == 31) {
            (target, call) =
                (address(station), abi.encodeCall(Station.bridge, ("local-bridge", 999, amount, "")));
        } else {
            (target, call) =
                (address(venue), abi.encodeCall(ILocalVenue.place, (address(station), _sell(amount))));
        }
        (ok,) = target.call(call);
    }

    /// A sell of the station's spot, at any price.
    function _sell(uint256 amount) internal pure returns (OrderRequest[] memory orders) {
        orders = new OrderRequest[](1);
        orders[0] =
            OrderRequest({market: SPOT, isBuy: false, size: amount * 1e12, limitPrice: 0, reduceOnly: true});
    }

    // The admin over time -----------------------------------------------------------

    /// The admin acting over time, as far as the registry lets it. It
    /// proposes an attestation checker, images and a project of its own, a
    /// working driver of its own under the vault's driver name, a taking
    /// driver and a keeping bridge, and shorter or longer timelocks; it
    /// waits the timelock out; it blesses keys it holds as this vault's
    /// executor and unwinder, through the platform's checker (the hardware
    /// honestly running the admin's own image) or through its own; and those
    /// keys then do everything an executor or an unwinder can.
    ///
    /// A driver is code that runs for every vault naming it, so one that
    /// hands a station's dollars away is for the timelock to stop: proposing
    /// it is public, and depositors have the timelock to leave before it
    /// applies. The walk proposes the taking driver only under a name this
    /// vault doesn't use; what it could do under the vault's own name is
    /// shown separately.
    ///
    /// Whatever the sequence, nothing the admin controls ever receives a
    /// dollar, and every dollar that leaves the vault is a payment one of its
    /// events announces, to whom it is owed and of what it is owed.
    function adminPlan(uint256 op, uint256 seed) public {
        uint256 k = op % 10;
        if (k >= 6) {
            _adminKeysAct(k, seed);
            return;
        }
        _begin();
        if (k >= 4) {
            vm.warp(block.timestamp + attestation.timelock() + 1);
            _renew();
            _hit("admin waited out the timelock");
        } else {
            bool ok;
            vm.startPrank(admin);
            if (k == 0) {
                // Its own checker, executor and unwinder images, and project.
                (ok,) = address(attestation).call(
                    abi.encodeCall(AttestationRegistry.proposeVerifier, (address(adminVerifier)))
                );
                (bool image,) = address(attestation).call(
                    abi.encodeCall(AttestationRegistry.proposeImage, (ADMIN_IMAGE, ImageKind.Executor))
                );
                (bool unwinder,) = address(attestation).call(
                    abi.encodeCall(
                        AttestationRegistry.proposeImage, (ADMIN_UNWINDER_IMAGE, ImageKind.Unwinder)
                    )
                );
                (bool project_,) = address(attestation).call(
                    abi.encodeCall(AttestationRegistry.proposeProject, (ADMIN_PROJECT))
                );
                ok = ok || image || unwinder || project_;
            } else if (k == 1) {
                (ok,) = address(attestation).call(
                    seed % 2 == 0
                        ? abi.encodeCall(AttestationRegistry.proposeDriver, ("local", address(adminDriver)))
                        : abi.encodeCall(AttestationRegistry.proposeDriver, ("other", address(takingDriver)))
                );
            } else if (k == 2) {
                (ok,) = address(attestation).call(
                    abi.encodeCall(
                        AttestationRegistry.proposeBridge,
                        (
                            seed % 2 == 0 ? bytes32("local-bridge") : bytes32("keeping-bridge"),
                            address(keepingBridge)
                        )
                    )
                );
            } else {
                (ok,) = address(attestation).call(
                    abi.encodeCall(AttestationRegistry.setTimelock, (uint64(_bound(seed, 0, 4 days))))
                );
            }
            vm.stopPrank();
            if (ok) _hit("admin proposed");
        }
        _end(true, "adminPlan");
        _nothingMoved("a proposal of the admin's moved money");
    }

    /// The admin's keys at work.
    function _adminKeysAct(uint256 k, uint256 seed) internal {
        if (k == 6) {
            _blessAdminKeys(seed);
            return;
        }
        trader = seed % 2 == 0 ? adminKey : adminImposter;
        uint256 a = seed >> 8;
        uint256 b = seed >> 72;
        // It keeps its keys blessed, and its executor key holding the venue,
        // where the registry lets it.
        _blessAs(adminKey, ADMIN_IMAGE, ADMIN_PROJECT, true);
        _blessAs(adminKey, ADMIN_IMAGE, ADMIN_PROJECT, false);
        _blessAs(adminUnwinder, ADMIN_UNWINDER_IMAGE, ADMIN_PROJECT, true);
        _blessAs(adminUnwinder, ADMIN_UNWINDER_IMAGE, ADMIN_PROJECT, false);
        _blessAs(adminImposter, executorImage, project, false);
        if (!station.stopped()) {
            vm.prank(trader);
            try station.approveKey("local", trader) {} catch {}
        }
        if (k == 7) {
            // Everything the executor does.
            executorAct(seed >> 160, a, b);
        } else if (k == 8) {
            // Its bills: kept by anyone, or collected as the author takes
            // from the tank.
            if (seed % 3 == 0) _withdrawTank(a, b);
            else _keep(a, true);
        } else {
            _begin();
            // The unwinder closes any fraction it likes, and the executor
            // key tries to bridge the station's dollars anywhere and to cut
            // the platform's executor off.
            uint256 fraction = _bound(a, 1, BPS) * WAD / BPS;
            vm.prank(adminUnwinder);
            try station.unwind(fraction) {
                still = false;
                _hit("unwind by the admin's unwinder");
            } catch {}
            uint256 amount = usd.balanceOf(address(station)) * _bound(b, 1, 100) / 100;
            vm.startPrank(trader);
            try station.bridge(
                seed % 4 < 2 ? bytes32("local-bridge") : bytes32("keeping-bridge"),
                seed % 2 == 0 ? 999 : block.chainid,
                amount,
                ""
            ) {
                _hit("bridge by an admin's key");
            } catch {}
            try station.revokeKey("local", executor) {
                _hit("admin's key revoked executor");
            } catch {}
            vm.stopPrank();
            _end(false, "adminKeys");
        }
        trader = executor;
    }

    /// Bless the admin's keys for this vault: its own executor image's key
    /// through the platform's checker or its own, its unwinder, and a key
    /// passed off as the platform's executor through its own checker. Each
    /// executor key then takes the venue.
    function _blessAdminKeys(uint256 seed) internal {
        _begin();
        _blessAs(adminKey, ADMIN_IMAGE, ADMIN_PROJECT, seed % 2 == 0);
        _blessAs(adminUnwinder, ADMIN_UNWINDER_IMAGE, ADMIN_PROJECT, seed % 3 == 0);
        _blessAs(adminImposter, executorImage, project, false);
        address taker = seed % 5 < 2 ? adminKey : seed % 5 < 4 ? adminImposter : address(0);
        if (taker != address(0) && !station.stopped()) {
            vm.prank(taker);
            try station.approveKey("local", taker) {
                _hit("admin's key took the venue");
            } catch {}
        }
        _end(true, "blessAdminKeys");
        _nothingMoved("blessing the admin's keys moved money");
    }

    /// A journal for `signer` running `image`, sealed by the platform's
    /// checker's key or the admin's own.
    function _blessAs(address signer, bytes32 image, bytes32 project_, bool platformChecker) internal {
        bytes32 leaf = keccak256(
            bytes.concat(keccak256(abi.encode(block.chainid, address(vault), bundle, block.chainid)))
        );
        bytes memory journal = abi.encode(
            AttestationRegistry.Journal({
                imageDigest: image,
                projectId: project_,
                signer: signer,
                vaultsRoot: leaf,
                issuedAt: uint64(block.timestamp)
            })
        );
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(platformChecker ? attestorKey : adminAttestorKey, _ethSigned(keccak256(journal)));
        // With a gas limit: a checker the walk proposed at a precompile's
        // address burns all the gas it is given.
        try attestation.bless{gas: 3_000_000}(
            abi.encodePacked(r, s, v), journal, block.chainid, address(vault), bundle, new bytes32[](0)
        ) {
            _hit(
                platformChecker
                    ? bytes32("admin key blessed by platform")
                    : bytes32("admin key blessed by its own")
            );
        } catch {}
    }

    /// The dollars every address the admin controls holds.
    function adminHoldings() external view returns (uint256 total) {
        for (uint256 i = 0; i < adminAddresses.length; i++) {
            total += usd.balanceOf(adminAddresses[i]);
        }
    }

    // Thought experiments ---------------------------------------------------------

    /// A newcomer deposits and leaves again at the same prices: they never
    /// get back more than they put in, at any size.
    function _roundTrip(uint256 amount) internal {
        if (vault.state() != VaultState.Open) return;
        amount = _bound(amount, 10e6, 1_000_000e6);
        uint256 snap = vm.snapshotState();
        (bool ran, uint256 back) = _runRoundTrip(amount);
        vm.revertToStateAndDelete(snap);
        vm.getRecordedLogs();
        if (!ran) return;
        _hit("roundTrip");
        if (back > amount) {
            roundTripBreaks++;
            _break(string.concat("a round trip of ", vm.toString(amount), " returned ", vm.toString(back)));
        }
    }

    function _runRoundTrip(uint256 amount) internal returns (bool, uint256) {
        if (!_quiet()) return (false, 0);
        usd.mint(newcomer, amount);
        vm.startPrank(newcomer);
        usd.approve(address(vault), amount);
        vault.requestDeposit(amount, newcomer, _statement(newcomer));
        vm.stopPrank();
        if (!_valueAndSettle()) return (false, 0);
        uint256 shares = vault.balanceOf(newcomer);
        if (shares == 0) return (true, usd.balanceOf(newcomer));
        vm.prank(newcomer);
        vault.requestRedeem(shares, newcomer);
        uint256 claims = vault.claimCount();
        if (!_valueAndSettle()) return (false, 0);
        if (vault.claimCount() != claims) {
            // The cash couldn't pay it: the newcomer closes their fraction.
            uint256 id = vault.claimCount();
            try vault.settleClaim(id) {} catch {}
            if (vault.claimAt(id).state == ClaimState.Queued) _valueAndSettle();
            if (vault.claimAt(id).state != ClaimState.Paid) return (false, 0);
        }
        if (vault.balanceOf(newcomer) != 0) return (false, 0);
        return (true, usd.balanceOf(newcomer));
    }

    /// A gain shown at one valuation and given back before payday mints
    /// nothing for it: payday mints exactly what it would have had the
    /// market never moved.
    function _gainGivenBack(uint256 moveBps, bool up) internal {
        VaultState state = vault.state();
        if ((state != VaultState.Open && state != VaultState.WindingDown) || vault.haltedAt() != 0) return;
        moveBps = _bound(moveBps, 100, 3000);
        uint256 snap = vm.snapshotState();
        (bool ran, uint256 withMove, uint256 without) = _runGainGivenBack(moveBps, up);
        vm.revertToStateAndDelete(snap);
        vm.getRecordedLogs();
        if (!ran) return;
        _hit("gainGivenBack");
        if (withMove > without) {
            givenBackBreaks++;
            _break(
                string.concat(
                    "a move given back minted ", vm.toString(withMove), " against ", vm.toString(without)
                )
            );
        }
    }

    function _runGainGivenBack(uint256 moveBps, bool up) internal returns (bool, uint256, uint256) {
        if (!_quiet()) return (false, 0, 0);
        // Something at the venue for the market to move.
        (uint256 markValue,) = station.value();
        if (markValue == usd.balanceOf(address(station))) return (false, 0, 0);
        uint256 t1 = _nextSlot();
        uint256 payday = _nextMonday(vault.lastPayment());
        if (t1 >= payday) return (false, 0, 0);
        uint256 t2 = payday.ceilDiv(SLOT) * SLOT;
        uint256 calm = vm.snapshotState();
        (bool ranMoved, uint256 moved) = _paydayAfter(t1, t2, moveBps, up);
        vm.revertToStateAndDelete(calm);
        (bool ranStill, uint256 stayed) = _paydayAfter(t1, t2, 0, up);
        return (ranMoved && ranStill, moved, stayed);
    }

    /// Value at `t1` with the market moved, move it back, and value at the
    /// payday `t2`: the fee shares payday mints.
    function _paydayAfter(
        uint256 t1,
        uint256 t2,
        uint256 moveBps,
        bool up
    ) internal returns (bool, uint256) {
        Market memory spot = venue.marketOf(SPOT);
        Market memory perp = venue.marketOf(PERP);
        if (moveBps != 0) {
            vm.startPrank(operator);
            _shift(SPOT, spot, moveBps, up);
            _shift(PERP, perp, moveBps, up);
            vm.stopPrank();
        }
        vm.warp(t1);
        try vault.recordValuation() {}
        catch {
            return (false, 0);
        }
        vm.startPrank(operator);
        venue.setPrices(SPOT, spot.mark, spot.oracle, spot.bid, spot.ask);
        venue.setPrices(PERP, perp.mark, perp.oracle, perp.bid, perp.ask);
        vm.stopPrank();
        vm.warp(t2);
        vm.recordLogs();
        try vault.recordValuation() {}
        catch {
            return (false, 0);
        }
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter == address(vault) && logs[i].topics[0] == IVault.FeesPaid.selector) {
                (uint256 toAuthor, uint256 toPlatform,,) =
                    abi.decode(logs[i].data, (uint256, uint256, uint256, uint256));
                return (true, toAuthor + toPlatform);
            }
        }
        return (false, 0);
    }

    function _shift(bytes32 market, Market memory m, uint256 moveBps, bool up) internal {
        uint256 f = up ? BPS + moveBps : BPS - moveBps;
        venue.setPrices(market, m.mark * f / BPS, m.oracle * f / BPS, m.bid * f / BPS, m.ask * f / BPS);
    }

    /// Let everyone out at the market as it stands: halt, stop, unwind,
    /// value, close, and pay every request, claim, holder and unit holder.
    /// Nothing may fail, the vault must end owing nothing but its tank and
    /// the dead shares' part, and nobody may have been paid more than the
    /// cash and the stations at exit held.
    function _payEveryoneOut() internal {
        VaultState state = vault.state();
        if (state == VaultState.Cancelled) return;
        // A market outside the band closes nothing: there is no exit to
        // judge until it is back.
        if (station.outsideBand()) return;
        uint256 snap = vm.snapshotState();
        (bool ran, string memory failure) = _runPayEveryoneOut();
        vm.revertToStateAndDelete(snap);
        vm.getRecordedLogs();
        if (!ran) return;
        _hit("payEveryoneOut");
        if (bytes(failure).length != 0) {
            payoutBreaks++;
            _break(failure);
        }
    }

    function _runPayEveryoneOut() internal returns (bool, string memory) {
        (, uint256 exit) = station.value();
        (, uint256 reserve) = vault.units();
        uint256 tank = vault.tank().balance;
        uint256 heldBefore = _heldTotal();
        uint256 available = vault.cash() + exit + vault.escrow() + reserve + heldBefore;
        uint256[] memory start = new uint256[](payees.length);
        for (uint256 i = 0; i < payees.length; i++) {
            start[i] = usd.balanceOf(payees[i]);
        }

        bool closed = vault.state() == VaultState.Closed;
        if (!closed) {
            try vault.settle(type(uint256).max) {}
            catch {
                return (true, "settle reverted");
            }
            if (vault.haltedAt() == 0) {
                vm.prank(admin);
                vault.kill();
            }
        }
        // Stop the station once the grace has passed and bring everything
        // home; a closed vault's station may still hold what a market gave
        // back after its last valuation.
        uint256 t = vault.haltedAt() + vault.haltGrace();
        if (t <= block.timestamp) t = block.timestamp + 1;
        vm.warp(t.ceilDiv(SLOT) * SLOT);
        if (!station.stopped()) {
            try station.stop() {}
            catch {
                return (true, "the station could not be stopped");
            }
        }
        (uint256 m, uint256 mx) = station.value();
        if (m != 0 || mx != 0) {
            try station.unwind(WAD) returns (uint256, bool blocked) {
                // A market that left the band since: nothing to judge.
                if (blocked) return (false, "");
            } catch (bytes memory reason) {
                return (true, string.concat("the full unwind reverted: ", vm.toString(reason)));
            }
        }
        if (!closed) {
            try vault.recordValuation() {}
            catch (bytes memory reason) {
                return (true, string.concat("the last valuation reverted: ", vm.toString(reason)));
            }
            try vault.settle(type(uint256).max) {}
            catch {
                return (true, "the last settle reverted");
            }
            try vault.close() {}
            catch (bytes memory reason) {
                return (true, string.concat("the close reverted: ", vm.toString(reason)));
            }
        }
        uint256 count = vault.requestCount();
        for (uint256 id = 1; id <= count; id++) {
            Request memory r = vault.requestAt(id);
            if (r.done || r.kind == RequestKind.Claim) continue;
            try vault.settleClosed(id) {}
            catch {
                return (true, string.concat("request ", vm.toString(id), " could not be settled closed"));
            }
        }
        count = vault.claimCount();
        for (uint256 id = 1; id <= count; id++) {
            ClaimState s = vault.claimAt(id).state;
            if (s != ClaimState.Pending && s != ClaimState.Waiting && s != ClaimState.Queued) continue;
            try vault.claimClosed(id) {}
            catch {
                return (true, string.concat("claim ", vm.toString(id), " could not be paid closed"));
            }
        }
        // Fee shares can reach the author and the platform inside this
        // experiment, where nobody notes them as holders.
        _noteHolder(platform);
        _noteHolder(author);
        for (uint256 i = 0; i < holders.length; i++) {
            address h = holders[i];
            uint256 shares = vault.balanceOf(h);
            if (shares != 0 && h != DEAD && h != address(vault)) {
                vm.prank(h);
                try vault.redeemClosed(shares, h) {}
                catch {
                    return (true, string.concat(vm.toString(h), " could not redeem from the closed vault"));
                }
            }
            (uint256 raw,) = vault.unitsOf(h);
            if (raw != 0) {
                try vault.serveProRata(h) {}
                catch (bytes memory reason) {
                    // An emptied station whose last valuation still saw dust
                    // owes the holder nothing more: what came home is in the
                    // reserve they collect from.
                    (, uint256 left) = station.value();
                    if (left != 0 || bytes4(reason) != IVault.NotYet.selector) {
                        return (
                            true,
                            string.concat(
                                vm.toString(h), "'s units could not be served: ", vm.toString(reason)
                            )
                        );
                    }
                }
            }
            (, uint256 claimable) = vault.unitsOf(h);
            if (claimable != 0) {
                vm.prank(h);
                try vault.collectUnits() {}
                catch (bytes memory reason) {
                    return (
                        true,
                        string.concat(
                            vm.toString(h), "'s units could not be collected: ", vm.toString(reason)
                        )
                    );
                }
            }
        }
        for (uint256 i = 0; i < payees.length; i++) {
            if (vault.heldFor(payees[i]) != 0) {
                vm.prank(payees[i]);
                try vault.collectHeld() {}
                catch {
                    return (true, "a held payout could not be collected");
                }
            }
        }
        (, reserve) = vault.units();
        if (vault.escrow() != 0) return (true, "escrow left after everyone was paid");
        if (vault.totalSupply() != vault.balanceOf(DEAD)) return (true, "shares left that nobody can redeem");
        if (reserve > 1e3) {
            return (
                true, string.concat("a unit reserve left after every unit was served: ", vm.toString(reserve))
            );
        }
        // The author's own fills may still land in the tank; nothing takes
        // from it.
        if (vault.tank().balance < tank) return (true, "paying everyone out took from the tank");
        if (usd.balanceOf(address(vault)) < vault.cash() + vault.tank().balance) {
            return (true, "insolvent after the payout");
        }
        uint256 paid;
        for (uint256 i = 0; i < payees.length; i++) {
            paid += usd.balanceOf(payees[i]) - start[i];
        }
        if (paid > available + 1e3) {
            return (
                true,
                string.concat(
                    "everyone was paid ",
                    vm.toString(paid),
                    " from cash and stations of ",
                    vm.toString(available)
                )
            );
        }
        return (true, "");
    }

    // The books, read by the invariants ------------------------------------------

    /// What the vault owes out of its own balance.
    function owed() external view returns (uint256) {
        (, uint256 reserve) = vault.units();
        return vault.cash() + vault.tank().balance + vault.escrow() + reserve + _heldTotal();
    }

    /// What every unit holder could collect now.
    function unitClaims() external view returns (uint256 total) {
        for (uint256 i = 0; i < holders.length; i++) {
            (, uint256 claimable) = vault.unitsOf(holders[i]);
            total += claimable;
        }
    }

    function holderCount() external view returns (uint256) {
        return holders.length;
    }

    /// The tank's balance as its own events tell it.
    function tankBooks() external view returns (uint256) {
        return tankStart + tankIn - tankCharged - tankOut;
    }

    /// The most every collection could have charged: the allowances in force
    /// from activation, priced at the published prices, up to now or to the
    /// halt plus its grace.
    function capSoFar() public view returns (uint256 cap) {
        uint64 end = _capEnd(uint64(block.timestamp));
        cap = _capBetween(vault.createdAt(), end);
    }

    function hitCount() external view returns (uint256) {
        return hitNames.length;
    }

    // Measuring a step --------------------------------------------------------------

    function _begin() internal {
        vm.getRecordedLogs();
        vm.recordLogs();
        outflows = 0;
        before.price = netPerShare();
        before.supply = vault.totalSupply();
        before.mark = vault.mark();
        before.vaultUsd = usd.balanceOf(address(vault));
        before.stationUsd = usd.balanceOf(address(station));
        before.venueCash = venue.cashOf(address(station));
        before.cash = vault.cash();
        before.escrow = vault.escrow();
        before.tank = vault.tank().balance;
        before.adminUsd = usd.balanceOf(admin);
        (, before.reserve) = vault.units();
        before.state = vault.state();
        delete owedOut;
        (stepPayee, stepRequest, stepClaimable) = (address(0), 0, 0);
        measured.set = false;
    }

    function _end(bool checkPrice, string memory what) internal {
        uint256 holdingMinted = _readLogs(what);

        // The mark never falls.
        uint256 m = vault.mark();
        if (m < lastMark) {
            markBreaks++;
            _break(string.concat(what, ": the mark fell"));
        }
        lastMark = m;

        // The tank never owes: usage only goes forward, and the balance is
        // exactly what came in less what was charged and taken out.
        Tank memory t = vault.tank();
        if (t.usageSettled < lastUsage) {
            tankBreaks++;
            _break(string.concat(what, ": the usage total went back"));
        }
        lastUsage = t.usageSettled;
        if (t.balance != tankStart + tankIn - tankCharged - tankOut) {
            tankBreaks++;
            _break(string.concat(what, ": the tank's balance is not its books"));
        }
        if (tankCharged > capSoFar() + allowanceSince.length) {
            tankBreaks++;
            _break(string.concat(what, ": the tank was charged above the allowances"));
        }

        // A passive holder's price never drops for anyone else's move,
        // beyond rounding and the holding fee the step synced.
        if (checkPrice) {
            _hit("~passive");
            uint256 afterwards = netPerShare();
            uint256 slack = before.price / 1e9 + 1e12;
            if (holdingMinted != 0) {
                slack += before.price.mulDiv(
                    holdingMinted, before.supply + OFFSET + holdingMinted, Math.Rounding.Ceil
                );
            }
            if (afterwards + slack < before.price) {
                passiveBreaks++;
                _break(
                    string.concat(
                        what,
                        ": the passive holder's price fell from ",
                        vm.toString(before.price),
                        " to ",
                        vm.toString(afterwards)
                    )
                );
            }
        }
    }

    struct Reading {
        uint256 supply;
        uint256 mark;
        uint256 holdingMinted;
        bool payday;
        uint256 minted;
        uint256 mintedSupply;
        uint256 markBefore;
    }

    /// What a step's dollars did, as the dollar's own transfers tell it,
    /// beside what the vault's events say: the two are matched at the end.
    struct Ledger {
        /// Every dollar that left the vault, and every payout held for a
        /// receiver who refused it instead: to whom and how much.
        Payment[] paid;
        uint256 count;
        /// The vault's cash before the step, less what its closed-vault
        /// payouts and fallbacks have paid so far.
        uint256 cash;
        /// What the station sent the vault this step, and what the vault's
        /// events say came home.
        uint256 home;
        uint256 booked;
        /// The last holder whose shares were burned, and how many.
        address burnFrom;
        uint256 burned;
        /// The withdrawal just announced, when the next event is the claim
        /// it paid.
        bool withdrawn;
        uint256 wId;
        address wOwner;
        uint256 wShares;
        uint256 wAssets;
        uint256 wSlice;
    }

    function _readLogs(string memory what) internal returns (uint256) {
        Vm.Log[] memory logs = vm.getRecordedLogs();
        Reading memory r = Reading({
            supply: before.supply,
            mark: before.mark,
            holdingMinted: 0,
            payday: false,
            minted: 0,
            mintedSupply: 0,
            markBefore: 0
        });
        Ledger memory g;
        g.paid = new Payment[](logs.length);
        g.cash = before.cash;
        for (uint256 i = 0; i < logs.length; i++) {
            Vm.Log memory l = logs[i];
            if (l.topics.length == 0) continue;
            bytes32 topic = l.topics[0];
            if (l.emitter == address(usd)) {
                if (topic == TRANSFER) _dollarMoved(l, g, what);
                continue;
            }
            if (l.emitter != address(vault)) continue;
            // Whether the event just before this one was a withdrawal: a
            // claim's payment is announced by the two together.
            bool afterWithdrawal = g.withdrawn;
            g.withdrawn = false;
            if (topic == TRANSFER) {
                address from = address(uint160(uint256(l.topics[1])));
                address to = address(uint160(uint256(l.topics[2])));
                uint256 amount = abi.decode(l.data, (uint256));
                if (from == address(0)) r.supply += amount;
                if (to == address(0)) {
                    r.supply -= amount;
                    (g.burnFrom, g.burned) = (from, amount);
                } else if (amount != 0) {
                    _noteHolder(to);
                }
            } else if (topic == IVault.HoldingSynced.selector) {
                uint256 minted = abi.decode(l.data, (uint256));
                _checkHolding(minted, r.supply - minted, what);
                unpaidHolding += minted;
                r.holdingMinted += minted;
            } else if (topic == IVault.FeesPaid.selector) {
                (uint256 toAuthor, uint256 toPlatform, uint256 holding, uint256 newMark) =
                    abi.decode(l.data, (uint256, uint256, uint256, uint256));
                // Payday hands over exactly the holding fee accrued.
                if (holding != unpaidHolding) {
                    feeBreaks++;
                    _break(string.concat(what, ": payday handed over holding shares nobody accrued"));
                }
                unpaidHolding = 0;
                r.payday = true;
                r.minted = toAuthor + toPlatform;
                r.mintedSupply = r.supply + OFFSET;
                r.markBefore = r.mark;
                r.mark = newMark;
            } else if (topic == IVault.MarkRaised.selector) {
                r.mark = abi.decode(l.data, (uint256));
            } else if (topic == IVault.Valued.selector) {
                _valued(l, r, what);
            } else if (topic == IVault.Withdrawn.selector) {
                _checkSlice(l, what);
                _withdrawn(logs, i, r, g, what);
            } else if (topic == IVault.ClaimPaid.selector) {
                _hit("~claimPaid");
                _claimPaid(l, r, g, afterWithdrawal, what);
            } else if (topic == IVault.Refunded.selector) {
                _hit("~refunded");
                _refunded(l, what);
            } else if (topic == IVault.ProRataTaken.selector) {
                _hit("~proRataTaken");
                _proRataTaken(l, r, g, what);
            } else if (topic == IVault.UnitsPaid.selector) {
                _hit("~unitsPaid");
                address owner = address(uint160(uint256(l.topics[1])));
                uint256 assets = abi.decode(l.data, (uint256));
                // Units are paid to the holder who collects them, what they
                // could collect.
                if (owner != stepPayee || assets != stepClaimable) {
                    _moneyBreak(
                        string.concat(what, ": units paid ", vm.toString(assets), " to the wrong holder")
                    );
                }
                _expect(owner, assets);
            } else if (topic == IVault.Held.selector) {
                _hit("~held");
                // Held for a receiver who refused it: paid, as far as the
                // event that announced it goes.
                g.paid[g.count++] =
                    Payment(address(uint160(uint256(l.topics[1]))), abi.decode(l.data, (uint256)));
            } else if (topic == IVault.TankFunded.selector) {
                tankIn += abi.decode(l.data, (uint256));
            } else if (topic == IVault.TankFilled.selector) {
                (, uint256 assets) = abi.decode(l.data, (uint256, uint256));
                tankIn += assets;
            } else if (topic == IVault.TankCharged.selector) {
                (, uint256 charged,) = abi.decode(l.data, (uint256, uint256, uint256));
                tankCharged += charged;
                _checkCharge(charged, what);
                // The running costs go to the platform.
                _expect(platform, charged);
            } else if (topic == IVault.TankWithdrawn.selector) {
                uint256 assets = abi.decode(l.data, (uint256));
                tankOut += assets;
                // The tank is the author's.
                _expect(author, assets);
            } else if (topic == IVault.ToStation.selector) {
                _expect(address(station), abi.decode(l.data, (uint256)));
            } else if (topic == IVault.FromStation.selector) {
                (uint256 assets,) = abi.decode(l.data, (uint256, uint256));
                g.booked += assets;
            } else if (topic == IVault.ClaimBooked.selector) {
                _hit("~claimBooked");
                _claimBooked(l, what);
            } else if (topic == IVault.ValuationVoided.selector) {
                _hit("~void");
            } else if (topic == IVault.Deposited.selector) {
                _hit("~deposited");
            } else if (topic == IVault.Requested.selector) {
                _entitle(address(uint160(uint256(l.topics[2]))));
                _requested(l, what);
            }
        }
        // A payday with no valuation after it is the close's, on its cash.
        if (r.payday) _checkPayday(r, vault.cash() + 1, what);
        _match(g, what);
        return r.holdingMinted;
    }

    /// One dollar transfer: where it went, and what it adds to the ledger.
    function _dollarMoved(Vm.Log memory l, Ledger memory g, string memory what) internal {
        address from = address(uint160(uint256(l.topics[1])));
        address to = address(uint160(uint256(l.topics[2])));
        uint256 amount = abi.decode(l.data, (uint256));
        _route(from, to, amount, what);
        if (amount == 0) return;
        // Nothing the admin controls is ever paid, whoever pays it.
        if (adminOwns[to] && from != address(0)) {
            adminBreaks++;
            _break(string.concat(what, ": ", vm.toString(amount), " reached the admin at ", vm.toString(to)));
        }
        if (from == address(vault)) {
            g.paid[g.count++] = Payment(to, amount);
            if (to == address(station)) pulledSince += amount;
        } else if (from == address(station) && to == address(vault)) {
            pushedSince += amount;
            g.home += amount;
        }
    }

    /// Every dollar that left the vault is one an event announced, to the
    /// one it names and of the amount it names, and every payment an event
    /// announced was made (or held for its receiver); and what came home from
    /// the station is what the vault booked.
    function _match(Ledger memory g, string memory what) internal {
        bool[] memory used = new bool[](g.count);
        for (uint256 k = 0; k < owedOut.length; k++) {
            Payment memory e = owedOut[k];
            if (e.amount == 0) continue;
            bool found;
            for (uint256 j = 0; j < g.count; j++) {
                if (!used[j] && g.paid[j].to == e.to && g.paid[j].amount == e.amount) {
                    (used[j], found) = (true, true);
                    break;
                }
            }
            if (!found) {
                _moneyBreak(
                    string.concat(
                        what,
                        ": an event says ",
                        vm.toString(e.to),
                        " was paid ",
                        vm.toString(e.amount),
                        " and nothing paid it"
                    )
                );
            }
        }
        for (uint256 j = 0; j < g.count; j++) {
            if (used[j]) continue;
            _hit("~unannounced");
            _moneyBreak(
                string.concat(
                    what,
                    ": ",
                    vm.toString(g.paid[j].amount),
                    " left the vault for ",
                    vm.toString(g.paid[j].to),
                    " and no event paid it"
                )
            );
        }
        if (g.home != g.booked) {
            _moneyBreak(
                string.concat(
                    what,
                    ": the station sent ",
                    vm.toString(g.home),
                    " home and the vault booked ",
                    vm.toString(g.booked)
                )
            );
        }
        if (g.count != 0) _hit("~matched");
    }

    /// A valuation: what it recorded is the pool the handler read just
    /// before it, on the supply the handler's own books hold, and the fee it
    /// owes is the fee rate on what is above the mark.
    function _valued(Vm.Log memory l, Reading memory r, string memory what) internal {
        uint256 index = uint256(l.topics[1]);
        (, uint256 gross, uint256 grossExit, uint256 supply,) =
            abi.decode(l.data, (uint64, uint256, uint256, uint256, uint256));
        lastPricedIndex = index + 1;
        still = true;
        (pulledSince, pushedSince) = (0, 0);
        if (measured.set) {
            _hit("~measured");
            if (gross != measured.gross || grossExit != measured.grossExit) {
                feeBreaks++;
                _break(
                    string.concat(
                        what,
                        ": a valuation recorded ",
                        vm.toString(gross),
                        " and ",
                        vm.toString(grossExit),
                        " for a pool of ",
                        vm.toString(measured.gross),
                        " and ",
                        vm.toString(measured.grossExit)
                    )
                );
            }
            stationsAtValuation = measured.stations;
            measured.set = false;
        } else {
            stationsAtValuation = gross - 1 - vault.valuationAt(index).cash;
        }
        if (supply != r.supply + OFFSET) {
            feeBreaks++;
            _break(string.concat(what, ": a valuation priced on a supply the shares don't add up to"));
        }
        Valuation memory v = vault.valuationAt(index);
        if (v.fee != _feeOn(gross, supply, r.mark) || v.feeExit != _feeOn(grossExit, supply, r.mark)) {
            feeBreaks++;
            _break(string.concat(what, ": a valuation owed a fee that isn't the rate on the gain"));
        }
        if (r.payday) {
            if (supply != r.mintedSupply) {
                feeBreaks++;
                _break(string.concat(what, ": payday priced on a supply it didn't mint to"));
            }
            _checkPayday(r, gross, what);
            r.payday = false;
        }
    }

    /// A withdrawal paid: to the one who asked, for the shares they asked to
    /// redeem, at the price of the valuation that priced it or, once the
    /// vault has closed, at the closed vault's price. A withdrawal followed
    /// by the claim it paid is judged with the claim.
    function _withdrawn(
        Vm.Log[] memory logs,
        uint256 i,
        Reading memory r,
        Ledger memory g,
        string memory what
    ) internal {
        Vm.Log memory l = logs[i];
        uint256 id = uint256(l.topics[1]);
        address owner = address(uint160(uint256(l.topics[2])));
        (uint256 shares, uint256 assets, uint256 slice) = abi.decode(l.data, (uint256, uint256, uint256));
        if (_nextVaultTopic(logs, i) == IVault.ClaimPaid.selector) {
            (g.withdrawn, g.wId, g.wOwner, g.wShares, g.wAssets, g.wSlice) =
                (true, id, owner, shares, assets, slice);
            return;
        }
        if (id == 0) {
            // A closed vault redeeming shares at once: the holder whose
            // shares were burned is the one paid.
            if (g.burnFrom != owner || g.burned != shares) {
                _moneyBreak(
                    string.concat(what, ": a closed vault paid someone other than the holder who redeemed")
                );
            }
            _closedPayout(assets, shares, r, g, what);
        } else {
            if (askedBy[id] != owner || askedKind[id] != RequestKind.Redeem || askedFor[id] != shares) {
                _moneyBreak(
                    string.concat(
                        what, ": withdrawal ", vm.toString(id), " was paid to someone other than who asked"
                    )
                );
            }
            _unask(owner, shares, what);
            if (before.state == VaultState.Closed) _closedPayout(assets, shares, r, g, what);
            else _pricedPayout(assets, shares, slice, what);
        }
        _expect(owner, assets);
    }

    /// A claim paid: to the one who booked it, for its shares, at its close
    /// (its fraction of what came home and of the cash, less its own fee and
    /// the transaction fee), at a valuation's price when the cash covered it,
    /// or at the closed vault's price.
    function _claimPaid(
        Vm.Log memory l,
        Reading memory r,
        Ledger memory g,
        bool afterWithdrawal,
        string memory what
    ) internal {
        uint256 claimId = uint256(l.topics[1]);
        uint256 assets = abi.decode(l.data, (uint256));
        address owner = claimOwner[claimId];
        uint256 shares = claimShares[claimId];
        if (afterWithdrawal) {
            if (g.wOwner != owner || g.wShares != shares || g.wAssets != assets) {
                _moneyBreak(
                    string.concat(
                        what, ": claim ", vm.toString(claimId), " was paid to someone other than its owner"
                    )
                );
            }
            if (g.wId == 0) {
                _hit("~claimClosed");
                _closeOfClaim(assets, shares, r, g, what);
            } else {
                _hit("~claimPriced");
                if (askedKind[g.wId] != RequestKind.Claim || claimOfRequest[g.wId] != claimId) {
                    _moneyBreak(string.concat(what, ": a claim was paid through another's request"));
                }
                _pricedPayout(assets, shares, g.wSlice, what);
            }
        } else {
            _closedPayout(assets, shares, r, g, what);
        }
        _expect(owner, assets);
    }

    /// At its close a claim is paid its shares' part of the cash as it stood
    /// and all the close brought home, less the profit fee those shares owe
    /// (paid to the author and the platform, ten to five) and the transaction
    /// fee.
    function _closeOfClaim(
        uint256 assets,
        uint256 shares,
        Reading memory r,
        Ledger memory g,
        string memory what
    ) internal {
        // The supply after the close's holding sync and before its burn.
        uint256 supply = r.supply + shares + OFFSET;
        uint256 owned = g.home + g.cash.mulDiv(shares, supply);
        if (owned > g.cash + g.home) owned = g.cash + g.home;
        uint256 fee = _feeOn(owned, shares, r.mark);
        uint256 gross = owned - fee;
        uint256 payout = gross - gross.mulDiv(vault.fees().transactionBps, BPS, Math.Rounding.Ceil);
        if (assets != payout) {
            _moneyBreak(
                string.concat(
                    what,
                    ": a claim's close paid ",
                    vm.toString(assets),
                    " where its fraction came to ",
                    vm.toString(payout)
                )
            );
        }
        if (fee != 0) {
            uint256 toAuthor =
                fee.mulDiv(vault.fees().authorBps, uint256(vault.fees().authorBps) + vault.fees().platformBps);
            _expect(author, toAuthor);
            _expect(platform, fee - toAuthor);
        }
    }

    /// What `shares` are paid at the last priced valuation's exit price,
    /// after the fee slice and the transaction fee.
    function _pricedPayout(uint256 assets, uint256 shares, uint256 slice, string memory what) internal {
        Valuation memory v = vault.valuationAt(lastPricedIndex - 1);
        uint256 owedSlice = v.feeExit == 0 ? 0 : shares.mulDiv(v.feeExit, v.grossExit, Math.Rounding.Ceil);
        uint256 gross = (shares - owedSlice).mulDiv(v.grossExit, v.supply);
        uint256 payout = gross - gross.mulDiv(vault.fees().transactionBps, BPS, Math.Rounding.Ceil);
        if (assets != payout || slice != owedSlice) {
            _moneyBreak(
                string.concat(
                    what,
                    ": a withdrawal paid ",
                    vm.toString(assets),
                    " where its shares came to ",
                    vm.toString(payout)
                )
            );
        }
    }

    /// A closed vault pays each share its part of the cash.
    function _closedPayout(
        uint256 assets,
        uint256 shares,
        Reading memory r,
        Ledger memory g,
        string memory what
    ) internal {
        // The supply just before these shares were burned.
        uint256 payout = g.cash.mulDiv(shares, r.supply + shares);
        if (assets != payout) {
            _moneyBreak(
                string.concat(
                    what,
                    ": a closed vault paid ",
                    vm.toString(assets),
                    " where the shares came to ",
                    vm.toString(payout)
                )
            );
        }
        g.cash -= payout > g.cash ? g.cash : payout;
    }

    /// A deposit sent back: to the one who asked, all of it.
    function _refunded(Vm.Log memory l, string memory what) internal {
        uint256 id = uint256(l.topics[1]);
        address owner = address(uint160(uint256(l.topics[2])));
        uint256 assets = abi.decode(l.data, (uint256));
        if (askedBy[id] != owner || askedKind[id] != RequestKind.Deposit || askedFor[id] != assets) {
            _moneyBreak(
                string.concat(what, ": deposit ", vm.toString(id), " was sent back to the wrong place")
            );
        }
        _expect(owner, assets);
    }

    /// The fallback: to the one whose request or claim it is, their shares'
    /// part of the cash as it stood, less the transaction fee.
    function _proRataTaken(Vm.Log memory l, Reading memory r, Ledger memory g, string memory what) internal {
        address owner = address(uint160(uint256(l.topics[1])));
        (uint256 shares, uint256 assets,) = abi.decode(l.data, (uint256, uint256, uint256));
        if (owner != stepPayee) {
            _moneyBreak(string.concat(what, ": the fallback paid someone other than the leaver"));
        }
        // The supply after the slice moved and before the rest was burned.
        uint256 gross = g.cash.mulDiv(shares, r.supply + shares + OFFSET);
        uint256 payout = gross - gross.mulDiv(vault.fees().transactionBps, BPS, Math.Rounding.Ceil);
        if (assets != payout) {
            _moneyBreak(
                string.concat(
                    what,
                    ": the fallback paid ",
                    vm.toString(assets),
                    " where the cash's share came to ",
                    vm.toString(payout)
                )
            );
        }
        g.cash -= payout > g.cash ? g.cash : payout;
        if (stepRequest != 0 && askedKind[stepRequest] == RequestKind.Redeem) {
            _unask(owner, askedFor[stepRequest], what);
        }
        _expect(owner, assets);
    }

    /// A claim booked: for someone who asked to redeem at least those shares.
    function _claimBooked(Vm.Log memory l, string memory what) internal {
        uint256 claimId = uint256(l.topics[1]);
        address owner = address(uint160(uint256(l.topics[2])));
        uint256 shares = abi.decode(l.data, (uint256));
        (claimOwner[claimId], claimShares[claimId]) = (owner, shares);
        if (
            stepRequest != 0
                && (
                    askedBy[stepRequest] != owner || askedFor[stepRequest] != shares
                        || askedKind[stepRequest] != RequestKind.Redeem
                )
        ) {
            _moneyBreak(string.concat(what, ": a claim was booked for someone other than who asked"));
        }
        _unask(owner, shares, what);
    }

    /// A request: as the handler asked it; or, asked by nobody the handler
    /// called for, the author's tank fill or a claim the cash covers, priced
    /// as a request.
    function _requested(Vm.Log memory l, string memory what) internal {
        uint256 id = uint256(l.topics[1]);
        address owner = address(uint160(uint256(l.topics[2])));
        (RequestKind kind,, uint256 amount) = abi.decode(l.data, (RequestKind, uint64, uint256));
        if (askedBy[id] != address(0)) {
            if (askedBy[id] != owner || askedFor[id] != amount || askedKind[id] != kind) {
                _moneyBreak(string.concat(what, ": request ", vm.toString(id), " is not what was asked"));
            }
            return;
        }
        if (kind == RequestKind.TankFill) {
            if (owner != author) _moneyBreak(string.concat(what, ": a tank fill for someone not the author"));
        } else if (kind == RequestKind.Claim) {
            uint256 claimId = vault.requestAt(id).ref;
            if (owner != claimOwner[claimId] || amount != claimShares[claimId]) {
                _moneyBreak(string.concat(what, ": a claim's request for someone other than its owner"));
            }
            claimOfRequest[id] = claimId;
        } else {
            _moneyBreak(string.concat(what, ": a request nobody asked for"));
        }
        _noteAsk(id, owner, amount, kind);
    }

    /// The topic of the vault's next event after `i`, if any.
    function _nextVaultTopic(Vm.Log[] memory logs, uint256 i) internal view returns (bytes32) {
        for (uint256 j = i + 1; j < logs.length; j++) {
            if (logs[j].emitter == address(vault) && logs[j].topics.length != 0) return logs[j].topics[0];
        }
        return bytes32(0);
    }

    /// The profit fee owed on `gross` for `supply` shares at mark `m`.
    function _feeOn(uint256 gross, uint256 supply, uint256 m) internal view returns (uint256) {
        uint256 basis = m.mulDiv(supply, 1e30, Math.Rounding.Ceil);
        if (gross <= basis) return 0;
        return (gross - basis).mulDiv(uint256(vault.fees().authorBps) + vault.fees().platformBps, BPS);
    }

    function _noteAsk(uint256 id, address who, uint256 amount, RequestKind kind) internal {
        (askedBy[id], askedFor[id], askedKind[id]) = (who, amount, kind);
    }

    /// Shares a holder asked to redeem have been paid, booked as a claim or
    /// taken through the fallback.
    function _unask(address owner, uint256 shares, string memory what) internal {
        if (redeeming[owner] < shares) {
            _moneyBreak(string.concat(what, ": shares were paid out that their owner never asked to redeem"));
            redeeming[owner] = 0;
        } else {
            redeeming[owner] -= shares;
        }
    }

    function _expect(address to, uint256 amount) internal {
        owedOut.push(Payment(to, amount));
    }

    function _moneyBreak(string memory why) internal {
        moneyBreaks++;
        _break(why);
    }

    /// Dollars leave the vault only for its station or someone it may pay;
    /// the station's only for the vault or its venue account; the venue's
    /// only for the station.
    function _route(address from, address to, uint256 amount, string memory what) internal {
        if (amount == 0) return;
        bool ok;
        if (from == address(vault)) {
            ok = to == address(station) || entitled[to];
        } else if (from == address(station)) {
            ok = to == address(vault) || to == address(venue);
        } else if (from == address(venue)) {
            ok = to == address(station);
        } else {
            return;
        }
        outflows++;
        _hit("~route");
        if (!ok) {
            moneyBreaks++;
            _break(
                string.concat(
                    what, ": ", vm.toString(amount), " went from ", vm.toString(from), " to ", vm.toString(to)
                )
            );
        }
    }

    /// The holding fee minted is at most the rate for the time since the
    /// last sync, as a dilution of every share.
    function _checkHolding(uint256 minted, uint256 supplyBefore, string memory what) internal {
        uint256 elapsed = block.timestamp - lastHolding;
        lastHolding = uint64(block.timestamp);
        uint256 rate = uint256(vault.fees().holdingBps) * elapsed * WAD / (BPS * YEAR);
        uint256 most = rate >= WAD ? type(uint256).max : (supplyBefore + OFFSET).mulDiv(rate, WAD - rate);
        if (minted > most) {
            feeBreaks++;
            _break(
                string.concat(
                    what, ": the holding fee minted ", vm.toString(minted), " over ", vm.toString(most)
                )
            );
        }
    }

    /// The fee shares a payday mints are worth, at the price after it, at
    /// most the profit fee owed on the value it was paid on.
    function _checkPayday(Reading memory r, uint256 gross, string memory what) internal {
        _hit(r.minted == 0 ? bytes32("~payday") : bytes32("~paydayMinted"));
        uint256 supply = r.mintedSupply - r.minted;
        uint256 basis = r.markBefore.mulDiv(supply, 1e30, Math.Rounding.Ceil);
        uint256 rate = uint256(vault.fees().authorBps) + vault.fees().platformBps;
        uint256 fee = gross > basis ? (gross - basis) * rate / BPS : 0;
        if (r.minted * gross > (fee + 1) * r.mintedSupply) {
            feeBreaks++;
            _break(
                string.concat(
                    what, ": payday minted ", vm.toString(r.minted), " shares for a fee of ", vm.toString(fee)
                )
            );
        }
    }

    /// A withdrawal's fee slice is at most its own shares' part of the fee
    /// owed at the valuation that priced it.
    function _checkSlice(Vm.Log memory l, string memory what) internal {
        uint256 requestId = uint256(l.topics[1]);
        (uint256 shares,, uint256 slice) = abi.decode(l.data, (uint256, uint256, uint256));
        if (requestId == 0 || slice == 0 || lastPricedIndex == 0) return;
        _hit("~slice");
        Valuation memory v = vault.valuationAt(lastPricedIndex - 1);
        if (slice * v.grossExit > shares * v.feeExit + v.grossExit) {
            feeBreaks++;
            _break(string.concat(what, ": a leaver's fee slice was more than their shares owed"));
        }
    }

    /// One collection charges at most the allowances' cost for the time it
    /// covers, from the last collection to its signed time, stopping at the
    /// halt plus its grace.
    function _checkCharge(uint256 charged, string memory what) internal {
        _hit(charged == 0 ? bytes32("~charge0") : bytes32("~charge"));
        uint64 start = lastCollection;
        uint64 end = _capEnd(billAsOf);
        if (end < start) end = start;
        uint256 cap = _capBetween(start, end);
        lastCollection = end;
        if (charged > cap + allowanceSince.length) {
            tankBreaks++;
            _break(
                string.concat(
                    what, ": a bill charged ", vm.toString(charged), " over a cap of ", vm.toString(cap)
                )
            );
        }
    }

    function _capEnd(uint64 at) internal view returns (uint64) {
        uint64 halted = vault.haltedAt();
        if (halted == 0) return at;
        uint64 stop = halted + vault.haltGrace();
        return at > stop ? stop : at;
    }

    function _capBetween(uint64 start, uint64 end) internal view returns (uint256 cap) {
        for (uint256 i = 0; i < allowanceSince.length; i++) {
            uint64 from = allowanceSince[i] > start ? allowanceSince[i] : start;
            uint64 to = i + 1 < allowanceSince.length ? allowanceSince[i + 1] : end;
            if (to > end) to = end;
            if (to > from) cap += prices.costOf(allowanceAt[i], from, to);
        }
    }

    /// A passive holder's worth per share: cash and the shares' part of the
    /// station at its mark, after the profit fee owed, scaled by 1e36.
    function netPerShare() public view returns (uint256) {
        (uint256 markValue,) = station.value();
        (uint256 units,) = vault.units();
        uint256 total = vault.cash() + markValue * (WAD - units) / WAD + 1;
        uint256 supply = vault.totalSupply() + OFFSET;
        uint256 basis = vault.mark().mulDiv(supply, 1e30, Math.Rounding.Ceil);
        uint256 rate = uint256(vault.fees().authorBps) + vault.fees().platformBps;
        uint256 fee = total > basis ? (total - basis) * rate / BPS : 0;
        return (total - fee) * 1e36 / supply;
    }

    // Helpers --------------------------------------------------------------------

    function _deposit(address who, uint256 amount) internal {
        usd.mint(who, amount);
        vm.startPrank(who);
        usd.approve(address(vault), amount);
        uint256 id = vault.requestDeposit(amount, who, _statement(who));
        vm.stopPrank();
        _noteAsk(id, who, amount, RequestKind.Deposit);
        _entitle(who);
    }

    /// Read the station as the vault is about to, then record a valuation:
    /// what the vault records must be the pool that was there.
    function _record() internal returns (bool ok) {
        (uint256 atMark, uint256 atExit) = station.value();
        (uint256 units,) = vault.units();
        if (atExit > atMark) atExit = atMark;
        uint256 cash = vault.cash();
        uint256 shares = atMark.mulDiv(WAD - units, WAD);
        measured = Measured({
            set: true,
            gross: cash + shares + 1,
            grossExit: cash + atExit.mulDiv(WAD - units, WAD) + 1,
            stations: shares
        });
        try vault.recordValuation() {
            ok = true;
        } catch {
            measured.set = false;
        }
    }

    /// Settle what is owed, then a valuation at the next slot and its
    /// settlement; false if anything refused.
    function _valueAndSettle() internal returns (bool) {
        vm.warp(_nextSlot());
        try vault.recordValuation() {}
        catch {
            return false;
        }
        if (vault.valuationAt(vault.valuationCount() - 1).status != ValuationStatus.Priced) {
            return false;
        }
        try vault.settle(type(uint256).max) {}
        catch {
            return false;
        }
        return true;
    }

    /// Bring the queue to rest: everything asked for so far is priced and
    /// settled at one valuation. False if the market won't price.
    function _quiet() internal returns (bool) {
        try vault.settle(type(uint256).max) {}
        catch {
            return false;
        }
        if (!_valueAndSettle()) return false;
        uint256 count = vault.requestCount();
        for (uint256 id = 1; id <= count; id++) {
            if (!vault.requestAt(id).done) return false;
        }
        return true;
    }

    function _renew() internal {
        bytes32 leaf = keccak256(
            bytes.concat(keccak256(abi.encode(block.chainid, address(vault), bundle, block.chainid)))
        );
        bytes memory journal = abi.encode(
            AttestationRegistry.Journal({
                imageDigest: executorImage,
                projectId: project,
                signer: executor,
                vaultsRoot: leaf,
                issuedAt: uint64(block.timestamp)
            })
        );
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(attestorKey, _ethSigned(keccak256(journal)));
        try attestation.bless(
            abi.encodePacked(r, s, v), journal, block.chainid, address(vault), bundle, new bytes32[](0)
        ) {} catch {}
        if (!station.stopped()) {
            vm.prank(executor);
            try station.approveKey("local", executor) {} catch {}
        }
    }

    function _statement(address who) internal view returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(keyOf[who], vault.statementDigest(who));
        return abi.encodePacked(r, s, v);
    }

    /// A running total signed by whichever key trades for the station.
    function _usage(uint256 total, uint64 asOf) internal view returns (bytes memory) {
        uint256 key = trader == executor ? executorKey : keyOf[trader];
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, _ethSigned(vault.usageDigest(total, asOf)));
        return abi.encodePacked(r, s, v);
    }

    function _ethSigned(bytes32 digest) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", digest));
    }

    /// The station's venue account is worth something, or holds nothing.
    function _venueSolvent() internal view returns (bool) {
        if (venue.accountValue(address(station)) != 0) return true;
        return venue.cashOf(address(station)) == 0 && venue.positionOf(address(station), SPOT).size == 0
            && venue.positionOf(address(station), PERP).size == 0;
    }

    function _floorNow() internal view returns (uint16) {
        if (pendingFloorAt != 0 && block.timestamp >= pendingFloorAt) return pendingFloorBps;
        return floorBps;
    }

    function _heldTotal() internal view returns (uint256 total) {
        for (uint256 i = 0; i < payees.length; i++) {
            total += vault.heldFor(payees[i]);
        }
    }

    function _actor(uint256 seed) internal view returns (address) {
        return actors[seed % actors.length];
    }

    function _unitHolder(uint256 seed) internal view returns (address) {
        uint256 n = holders.length;
        for (uint256 i = 0; i < n; i++) {
            address h = holders[(seed % n + i) % n];
            (uint256 raw, uint256 claimable) = vault.unitsOf(h);
            if (raw != 0 || claimable != 0) return h;
        }
        return address(0);
    }

    /// A request not yet done, starting from a random place in the queue.
    function _openRequest(uint256 seed) internal view returns (uint256) {
        uint256 n = vault.requestCount();
        for (uint256 i = 0; i < n && i < 32; i++) {
            uint256 id = 1 + (seed % n + i) % n;
            if (!vault.requestAt(id).done) return id;
        }
        return 0;
    }

    /// A claim not yet paid or taken.
    function _openClaim(uint256 seed) internal view returns (uint256) {
        uint256 n = vault.claimCount();
        for (uint256 i = 0; i < n && i < 32; i++) {
            uint256 id = 1 + (seed % n + i) % n;
            ClaimState s = vault.claimAt(id).state;
            if (s == ClaimState.Pending || s == ClaimState.Waiting || s == ClaimState.Queued) return id;
        }
        return 0;
    }

    function _nextSlot() internal view returns (uint256) {
        uint256 t = block.timestamp;
        return t - (t % SLOT) + SLOT;
    }

    function _nextMonday(uint256 t) internal pure returns (uint256) {
        uint256 sinceMonday = t / 1 days + 3;
        return (sinceMonday - (sinceMonday % 7) + 4) * 1 days;
    }

    function _noteHolder(address who) internal {
        if (!held[who]) {
            held[who] = true;
            holders.push(who);
        }
        _entitle(who);
    }

    function _entitle(address who) internal {
        if (who == address(vault) || who == DEAD || entitled[who]) return;
        entitled[who] = true;
        payees.push(who);
    }

    function _hit(bytes32 name) internal {
        if (hits[name] == 0) hitNames.push(name);
        hits[name]++;
    }

    /// A call that reverted leaves nothing behind, but the logs it emitted
    /// before reverting are still recorded: drop them, so the books never
    /// count what was undone. Only for a step whose one call it was.
    function _dropReverted() internal {
        vm.getRecordedLogs();
    }

    function _break(string memory why) internal {
        lastBreak = why;
    }
}
