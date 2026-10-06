// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Call, Custody, ExitPath, IDelayedDriver, IDriver, ValuationKind} from "../../interfaces/IDriver.sol";
import {IStation} from "../../interfaces/IStation.sol";
import {DelayedVenue} from "../DelayedVenue.sol";
import {LighterBoard, LighterPosition, LighterPrice, LighterReport, LighterShares} from "./LighterBoard.sol";
import {IZkLighter} from "./ZkLighter.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// Lighter on Robinhood Chain: the station owns its accounts at the exchange,
/// gives the executor a key that can trade them, and closes and withdraws
/// by itself, with priority requests.
///
/// The station's first deposit makes it the owner of a new master account.
/// It registers the executor's Lighter key (named on the board by the
/// executor's own key) on-chain, and replaces it the same way. The key can
/// trade, make sub-accounts and move collateral among the station's own
/// accounts, and ask for a withdrawal, which pays only the station; sending
/// anything to anyone else needs the owner's wallet signature, which a
/// station never gives (it has no signature function). So nothing leaves the
/// station's accounts but to the station.
///
/// What the accounts are worth exists only in the exchange's API, so their
/// value is an attested read: the newest post on the board. A post older
/// than `maxReportAge` voids a valuation, as does any held market whose mark
/// or book sits further from the exchange's index than the band, and any
/// post made before a deposit could have been credited.
///
/// Dollars move between the accounts and the station after a post: a
/// withdrawal is carried out into a pending balance, which anyone may pay
/// out to the station, and a deposit leaves the station at once. The post
/// carries the station's own counters as of its reading (every dollar that
/// had reached it from outside its calls, every dollar it had sent the
/// exchange), so whatever has moved since is counted exactly once: the
/// exchange's side is the post's equity and pending balance, plus what was
/// sent since, less what arrived since. Payouts are never made inside a
/// station call, so every one of them shows in the counter.
///
/// The exchange carries out the station's requests after the transaction, so
/// a claim's close is staged (`ExitPath.Delayed`): reduce-only orders are
/// placed, sized from a post at most `STAGE_POST_AGE` old; a post read at
/// least `SETTLE_DELAY` after the stage proves how much of them happened and
/// measures what the close fetched (a post read earlier says nothing about
/// it), then that is withdrawn and, minutes later, paid to the station. A
/// claim's close is refused before anything is sold when the accounts
/// couldn't release the fraction's worth after it, by the post's available
/// balances.
///
/// A stopped station nobody can post for any more closes everything against
/// its last post's index, inside the band as it widens, but that post prices
/// nothing: valuations stay void. Once the band is full (any price), an
/// unwind at that band has asked for everything, and a delivery window has
/// passed since the last thing it asked, whatever is still at the exchange
/// is given up as uncountable: the exchange's side then counts as only the
/// pending balance on-chain, which anyone pays out to the station, so the
/// vault can find its station empty and close.
///
/// It is immutable and has no owner. Its markets are fixed at deployment,
/// with their decimals as the exchange lists them.
contract LighterDriver is IDriver, IDelayedDriver {
    using Math for uint256;

    uint256 private constant WAD = 1e18;
    uint256 private constant BPS = 10_000;
    /// USDG is asset 3 at the exchange, ticked in its own smallest unit.
    uint16 public constant DOLLAR_ASSET = 3;
    uint8 private constant PERPS = 0;
    uint8 private constant MARKET_ORDER = 1;
    uint32 private constant MAX_PRICE = type(uint32).max;
    /// The key slot the executor's key goes in. Slots 0 and 1 are the
    /// exchange's own apps'.
    uint8 public constant KEY_SLOT = 2;
    /// A withdrawal is paid out six to nine minutes after it is asked for;
    /// half an hour holds a staged close's proof and its dollars with room.
    uint64 public constant DELIVERY_WINDOW = 30 minutes;
    /// How long a deposit may take to be credited to the account: a post
    /// read sooner after one may not show it, so it prices nothing.
    uint64 public constant DEPOSIT_CREDIT = 10 minutes;
    /// How long the exchange may take to carry out a priority request (an
    /// order or a key change takes seconds): a post proves a close only if
    /// read at least this long after it was placed.
    uint64 public constant SETTLE_DELAY = 2 minutes;
    /// The oldest post a claim's closing orders are sized from: the executor
    /// posts every minute, and an order sized from an older reading may
    /// close more of a position than the fraction, if the strategy has
    /// traded since.
    uint64 public constant STAGE_POST_AGE = 3 minutes;
    /// What the freed margin is counted at when judging whether a claim's
    /// close could come home: nine tenths.
    uint256 private constant SAFETY_BPS = 1000;
    /// The name stations know the driver by.
    bytes32 private constant NAME = "lighter";
    /// What a withdrawn key is replaced with: five field elements nobody
    /// holds the secret for (each below the field's modulus, not all zero).
    bytes public constant NOBODY =
        hex"01000000000000000200000000000000030000000000000004000000000000000500000000000000";

    struct Market {
        uint16 index;
        uint8 sizeDecimals;
        uint8 priceDecimals;
    }

    IZkLighter public immutable exchange;
    IERC20 public immutable dollar;
    LighterBoard public immutable board;
    /// Older than this, a post voids a valuation.
    uint256 public immutable maxReportAge;
    /// 10^dollar decimals.
    uint256 private immutable _dollarUnit;

    Market[] private _markets;

    error Repeated();
    error NoPubKey();

    constructor(
        IZkLighter exchange_,
        IERC20 dollar_,
        LighterBoard board_,
        uint256 maxReportAge_,
        uint8 dollarDecimals,
        Market[] memory markets_
    ) {
        exchange = exchange_;
        dollar = dollar_;
        board = board_;
        maxReportAge = maxReportAge_;
        _dollarUnit = 10 ** dollarDecimals;
        for (uint256 i = 0; i < markets_.length; i++) {
            for (uint256 j = 0; j < i; j++) {
                if (markets_[j].index == markets_[i].index) revert Repeated();
            }
            _markets.push(markets_[i]);
        }
    }

    // What it is ------------------------------------------------------------

    function name() external pure returns (bytes32) {
        return NAME;
    }

    function targets() external view returns (address[] memory list) {
        list = new address[](1);
        list[0] = address(exchange);
    }

    /// Who Lighter's terms exclude: residents and entities of Canada, China,
    /// the UK, Russia, Ukraine and the US, and sanctioned persons. Sorted.
    function restricted() external pure returns (bytes32[] memory list) {
        list = new bytes32[](7);
        list[0] = "CA";
        list[1] = "CN";
        list[2] = "GB";
        list[3] = "RU";
        list[4] = "SANCTIONED";
        list[5] = "UA";
        list[6] = "US";
    }

    function custody() external pure returns (Custody) {
        return Custody.Venue;
    }

    function exitPath() external pure returns (ExitPath) {
        return ExitPath.Delayed;
    }

    function deliveryWindow() external pure returns (uint64) {
        return DELIVERY_WINDOW;
    }

    function settleDelay() external pure returns (uint64) {
        return SETTLE_DELAY;
    }

    /// Trusted when there is no account, or the newest post is fresh and
    /// read after the last deposit was credited, or the station is stopped
    /// and has given up what it can't count (see the contract's note).
    function priced(address station) external view returns (bool) {
        (, bool exists) = masterOf(station);
        if (!exists) return true;
        (LighterReport memory report, bool reported) = board.reportOf(station);
        if (!reported) return false;
        if (_fresh(report)) return _credited(station, report);
        return _givenUp(station);
    }

    /// A stopped station whose post is stale, its band full, an unwind at
    /// that band made, and a delivery window passed since it last asked for
    /// anything.
    function _givenUp(address station) private view returns (bool) {
        IStation s = IStation(station);
        if (!s.stopped() || s.stoppedBand() < BPS) return false;
        uint256 band = s.spec().exitBandBps;
        if (band == 0) band = 1;
        uint256 doublings;
        while (band << doublings < BPS && doublings < 14) doublings++;
        uint256 fullAt = uint256(s.stoppedAt()) + doublings * s.WIDEN_AFTER();
        uint256 moved = s.movedAt(_name(station));
        return moved >= fullAt && block.timestamp >= moved + DELIVERY_WINDOW;
    }

    function _name(address station) private view returns (bytes32) {
        return DelayedVenue.nameIn(station, address(this));
    }

    function valuation() external pure returns (ValuationKind) {
        return ValuationKind.AttestedRead;
    }

    function markets() external view returns (Market[] memory) {
        return _markets;
    }

    /// The station's master account at the exchange, if it has one.
    function masterOf(address station) public view returns (uint48 account, bool exists) {
        account = exchange.addressToAccountIndex(station);
        exists = account != 0;
    }

    // Keys ------------------------------------------------------------------

    /// Put the key's Lighter key in the executor's slot of every account the
    /// station owns. Before the first deposit there is no account yet, and
    /// nothing to do: the executor approves its key again once there is.
    function approveKey(address station, address key, uint64) external view returns (Call[] memory calls) {
        bytes memory pubKey = board.pubKeyOf(key);
        if (pubKey.length != 40) revert NoPubKey();
        (, bool exists) = masterOf(station);
        if (!exists) return new Call[](0);
        return _keyCalls(station, pubKey, false);
    }

    /// Take the key out of every account the station owns, and cancel its
    /// resting orders there: an executor replaced must not keep trading.
    function revokeKey(address station, address) external view returns (Call[] memory) {
        (, bool exists) = masterOf(station);
        if (!exists) return new Call[](0);
        return _keyCalls(station, NOBODY, true);
    }

    // Money -----------------------------------------------------------------

    /// Into the station's master account, made by this deposit if it is the
    /// first. A key approved before then is registered by approving it again
    /// once the account exists: the exchange names a new account only as the
    /// deposit lands, so no call prepared before it can address the account.
    /// Deposits are spaced (`DelayedVenue`): each voids valuations until a
    /// post shows it credited.
    function deposit(address station, uint256 amount) external view returns (Call[] memory calls) {
        (, uint64 depositedAt) = IStation(station).venueFlows(_name(station));
        DelayedVenue.spaced(station, depositedAt, DEPOSIT_CREDIT);
        calls = new Call[](2);
        calls[0] = Call(address(dollar), 0, abi.encodeCall(IERC20.approve, (address(exchange), amount)));
        calls[1] = Call(
            address(exchange), 0, abi.encodeCall(IZkLighter.deposit, (station, DOLLAR_ASSET, PERPS, amount))
        );
    }

    /// Ask for `amount` from the accounts, never more of one than the
    /// newest post says it can give. What the exchange carries out lands as
    /// a pending balance minutes later, which anyone may then pay out to the
    /// station; never inside a station call, so the payout is counted.
    function withdraw(address station, uint256 amount) external view returns (Call[] memory) {
        return _withdrawCalls(station, amount);
    }

    /// The same; asked for everything (a stopped station's unwind), it asks
    /// for what the accounts can hold in descending halves, each its own
    /// request, so the requests the exchange can carry out take all but the
    /// last dollar whether it refuses or trims a request larger than the
    /// account (which of the two it does is not known here).
    function sweep(address station, uint256 amount) external view returns (Call[] memory) {
        return _withdrawCalls(station, amount);
    }

    // Closing ---------------------------------------------------------------

    /// Reduce-only orders closing `fraction` of every position, each no worse
    /// than the band from the exchange's index. Blocked when the newest post
    /// is too old or any held market is outside the band. At the full band,
    /// every listed market of every account is closed whole at any price,
    /// needing no post: a reduce-only order of zero size is the whole
    /// position, and the side that holds nothing does nothing. A stopped
    /// station whose newest post is stale (nobody can post once its keys
    /// are gone) closes everything whole too, each order no worse than the
    /// band from that post's index, as the band widens with time.
    function close(
        address station,
        uint256 fraction,
        uint16 bandBps
    ) external view returns (Call[] memory calls, bool blocked) {
        (uint48 master, bool exists) = masterOf(station);
        if (!exists) return (new Call[](0), false);
        (LighterReport memory report, bool reported) = board.reportOf(station);
        if (bandBps >= BPS) return (_closeEverything(master, report, 0), false);
        if (reported && !_fresh(report) && fraction == WAD && IStation(station).stopped()) {
            return (_closeEverything(master, report, bandBps), false);
        }
        if (!reported || !_fresh(report) || !_credited(station, report)) return (new Call[](0), true);
        if (_outsideBand(report, bandBps)) return (new Call[](0), true);
        // A claim's close: sized only from a recent post, and only if what it
        // frees could come home.
        if (
            fraction < WAD
                && (block.timestamp - report.asOf > STAGE_POST_AGE || !_releasable(station, report, fraction))
        ) return (new Call[](0), true);

        calls = new Call[](report.positions.length);
        uint256 made;
        for (uint256 i = 0; i < report.positions.length; i++) {
            LighterPosition memory position = report.positions[i];
            if (position.size == 0) continue;
            bool long = position.size > 0;
            uint256 open = uint256(uint64(long ? position.size : -position.size));
            uint256 size = open.mulDiv(fraction, WAD, Math.Rounding.Ceil);
            if (size > open) size = open;
            if (size == 0) continue;
            uint256 limit = _limit(_priceOf(report, position.market).index, long, bandBps);
            calls[made++] = _order(position.account, position.market, size, limit, long);
        }
        assembly ("memory-safe") {
            mstore(calls, made)
        }
    }

    /// The exchange's side from the newest post (however old: a stale post
    /// voids the valuation through `priced`): its equity and the pending
    /// balance it saw, plus what the station has sent the exchange since,
    /// less what has reached the station since. A withdrawal carried out
    /// since the post is still in its equity, and once paid out it is in the
    /// station's own dollars and taken off here, so nothing counts twice;
    /// a deposit since counts at once. The exit side takes each position at
    /// the book's closing side where that is worse than the mark.
    function valueOf(address station) external view returns (uint256 mark, uint256 exit) {
        (LighterReport memory report, bool reported) = board.reportOf(station);
        if (!reported || (!_fresh(report) && _givenUp(station))) {
            uint256 pending = exchange.getPendingBalance(station, DOLLAR_ASSET);
            return (pending, pending);
        }
        int256 worth = _worth(station, report);
        mark = worth > 0 ? uint256(worth) : 0;
        uint256 haircut;
        for (uint256 i = 0; i < report.positions.length; i++) {
            LighterPosition memory position = report.positions[i];
            if (position.size == 0) continue;
            (bool listed, Market memory market) = _market(position.market);
            LighterPrice memory price = _priceOf(report, position.market);
            if (!listed || price.mark == 0) continue;
            bool long = position.size > 0;
            uint256 open = uint256(uint64(long ? position.size : -position.size));
            uint256 worse = long
                ? (price.bid < price.mark ? price.mark - price.bid : 0)
                : (price.ask > price.mark ? price.ask - price.mark : 0);
            haircut +=
                open.mulDiv(worse * _dollarUnit, 10 ** (uint256(market.sizeDecimals) + market.priceDecimals));
        }
        exit = mark > haircut ? mark - haircut : 0;
    }

    /// The exchange's side, signed: the post's equity and pending balance,
    /// plus what was sent since it, less what arrived since it.
    function _worth(address station, LighterReport memory report) private view returns (int256) {
        (uint256 sent,) = IStation(station).venueFlows(_name(station));
        uint256 arrived = IStation(station).arrivals();
        int256 worth = int256(report.equity + report.pending);
        if (sent > report.sent) worth += int256(sent - report.sent);
        if (arrived > report.arrived) worth -= int256(arrived - report.arrived);
        return worth;
    }

    /// Whether a post was read long enough after the last deposit for the
    /// exchange to have credited it.
    function _credited(address station, LighterReport memory report) private view returns (bool) {
        (, uint64 depositedAt) = IStation(station).venueFlows(_name(station));
        return depositedAt == 0 || report.asOf >= uint256(depositedAt) + DEPOSIT_CREDIT;
    }

    // Delayed closes --------------------------------------------------------

    /// Whether the accounts could release `fraction` of the exchange's side
    /// after a close of `fraction`, by the post: what they can withdraw now,
    /// plus `fraction` of the margin the rest holds (counted at nine
    /// tenths), must cover `fraction` of the side's worth.
    function _releasable(
        address station,
        LighterReport memory report,
        uint256 fraction
    ) private view returns (bool) {
        int256 worth = _worth(station, report);
        if (worth <= 0) return true;
        uint256 free;
        for (uint256 i = 0; i < report.available.length; i++) {
            free += report.available[i];
        }
        uint256 margin = report.equity > free ? report.equity - free : 0;
        free += margin.mulDiv(fraction, WAD).mulDiv(BPS - SAFETY_BPS, BPS);
        return free >= uint256(worth).mulDiv(fraction, WAD, Math.Rounding.Ceil);
    }

    /// Each listed market's net position over all the station's accounts, in
    /// the driver's market order, then the post's time, then each market's
    /// mark in the post, then each position the post names, account by
    /// account, as its account and market (`account << 16 | market`) and
    /// its size: what `closedPart` compares a later post against, and what
    /// `measure` reprices.
    function exposure(address station) public view returns (int256[] memory held) {
        (LighterReport memory report,) = board.reportOf(station);
        return _exposure(report);
    }

    function _exposure(LighterReport memory report) private view returns (int256[] memory held) {
        uint256 n = _markets.length;
        held = new int256[](2 * n + 1 + 2 * report.positions.length);
        uint256 k = 2 * n + 1;
        for (uint256 i = 0; i < report.positions.length; i++) {
            LighterPosition memory position = report.positions[i];
            for (uint256 m = 0; m < n; m++) {
                if (_markets[m].index == position.market) held[m] += position.size;
            }
            held[k++] = int256(uint256(position.account) << 16 | position.market);
            held[k++] = position.size;
        }
        held[n] = int256(uint256(report.asOf));
        for (uint256 m = 0; m < n; m++) {
            held[n + 1 + m] = int256(uint256(_priceOf(report, _markets[m].index).mark));
        }
    }

    /// The exchange's side (`valueOf`'s, signed) less its positions at their
    /// marks: the accounts' dollars, by the newest post.
    function ledger(address station) external view returns (int256 dollars) {
        (LighterReport memory report,) = board.reportOf(station);
        dollars = _worth(station, report);
        int256[] memory held = _exposure(report);
        for (uint256 m = 0; m < _markets.length; m++) {
            dollars -= _notional(m, held[m], held[_markets.length + 1 + m]);
        }
    }

    /// By the newest post (read after the stage's settle delay, as
    /// `closedPart` asks): the exchange's side now, less the rest
    /// (1 - `fraction` x `part`, rounded up) of `dollars` and `before`'s
    /// positions repriced at the marks now, never below `bandBps` of what
    /// the close ordered at those marks. A market the newest post has no
    /// price for keeps the mark it had.
    function measure(
        address station,
        int256 dollars,
        int256[] calldata before,
        uint256 fraction,
        uint256 part,
        uint16 bandBps
    ) external view returns (int256) {
        (LighterReport memory report,) = board.reportOf(station);
        uint256 n = _markets.length;
        int256 then = dollars;
        int256[] memory marks = new int256[](n);
        for (uint256 m = 0; m < n; m++) {
            marks[m] = int256(uint256(_priceOf(report, _markets[m].index).mark));
            if (marks[m] == 0) marks[m] = before[n + 1 + m];
            then += _notional(m, before[m], marks[m]);
        }
        uint256 size = uint256(then < 0 ? -then : then);
        uint256 kept = size.mulDiv(
            WAD - fraction.mulDiv(part, WAD), WAD, then < 0 ? Math.Rounding.Floor : Math.Rounding.Ceil
        );
        int256 measured = _worth(station, report) - (then < 0 ? -int256(kept) : int256(kept));
        // What the orders could have lost at their limits: `bandBps` of what
        // they ordered, position by position.
        int256 least;
        for (uint256 k = 2 * n + 1; k + 1 < before.length; k += 2) {
            uint16 market = uint16(uint256(before[k]));
            for (uint256 m = 0; m < n; m++) {
                if (_markets[m].index != market) continue;
                int256 was = before[k + 1];
                uint256 ordered = uint256(was < 0 ? -was : was).mulDiv(fraction, WAD, Math.Rounding.Ceil);
                least -= int256(
                    uint256(_notional(m, int256(ordered), marks[m])).mulDiv(
                        Math.min(bandBps, BPS), BPS, Math.Rounding.Ceil
                    )
                );
            }
        }
        return measured < least ? least : measured;
    }

    /// `size` base units of market `m` at `price` ticks, in dollar units.
    function _notional(uint256 m, int256 size, int256 price) private view returns (int256) {
        Market memory market = _markets[m];
        return size * price * int256(_dollarUnit)
            / int256(10 ** (uint256(market.sizeDecimals) + market.priceDecimals));
    }

    /// Nothing is known of the close until a post read at or after `since`
    /// (the stage plus its settle delay). By that post, for each position
    /// the stage's post named (account by account, so a long in one and a
    /// short in another each answer for their own orders), the share of
    /// what was ordered that it is smaller by, on the same side or flat,
    /// rounded down; the smallest of them.
    function closedPart(
        address station,
        int256[] calldata before,
        uint256 fraction,
        uint64 since
    ) external view returns (uint256 part, bool known) {
        (LighterReport memory report, bool reported) = board.reportOf(station);
        if (!reported || report.asOf < since) return (0, false);
        part = WAD;
        known = true;
        uint256 n = _markets.length;
        for (uint256 k = 2 * n + 1; k + 1 < before.length; k += 2) {
            int256 was = before[k + 1];
            if (was == 0) continue;
            uint256 key = uint256(before[k]);
            int256 is_;
            for (uint256 i = 0; i < report.positions.length; i++) {
                LighterPosition memory position = report.positions[i];
                if (uint256(position.account) << 16 | position.market == key) is_ += position.size;
            }
            uint256 open = uint256(was > 0 ? was : -was);
            uint256 ordered = open.mulDiv(fraction, WAD, Math.Rounding.Ceil);
            if (ordered > open) ordered = open;
            if (ordered == 0) continue;
            // Crossed to the other side: more than closed, by its own trade.
            int256 done =
                (was > 0) == (is_ > 0) || is_ == 0 ? (was > 0 ? was - is_ : is_ - was) : was > 0 ? was : -was;
            if (done <= 0) return (0, true);
            uint256 share = uint256(done) >= ordered ? WAD : uint256(done).mulDiv(WAD, ordered);
            if (share < part) part = share;
        }
    }

    /// What the shares in the public pools named (each pool index as an
    /// address) are worth, by the newest post.
    function heldValue(address station, address[] calldata held) external view returns (uint256 value) {
        (LighterReport memory report,) = board.reportOf(station);
        for (uint256 h = 0; h < held.length; h++) {
            uint48 pool = uint48(uint160(held[h]));
            for (uint256 i = 0; i < report.pools.length; i++) {
                if (report.pools[i].pool == pool) value += report.pools[i].value;
            }
        }
    }

    /// Burn every share the station's accounts hold in pool `held` (the
    /// pool's index as an address), and say what they are worth.
    function reclaim(
        address station,
        address held
    ) external view returns (Call[] memory calls, uint256 equity) {
        (LighterReport memory report,) = board.reportOf(station);
        uint48 pool = uint48(uint160(held));
        calls = new Call[](report.pools.length);
        uint256 made;
        for (uint256 i = 0; i < report.pools.length; i++) {
            LighterShares memory shares = report.pools[i];
            if (shares.pool != pool || shares.shares == 0) continue;
            equity += shares.value;
            calls[made++] = Call(
                address(exchange),
                0,
                abi.encodeCall(IZkLighter.burnShares, (shares.account, pool, shares.shares))
            );
        }
        assembly ("memory-safe") {
            mstore(calls, made)
        }
    }

    // Internals -------------------------------------------------------------

    /// `pubKey` into the executor's slot of the master and every account the
    /// newest post names; with `cancel`, their resting orders cancelled too.
    function _keyCalls(
        address station,
        bytes memory pubKey,
        bool cancel
    ) private view returns (Call[] memory calls) {
        (uint48 master,) = masterOf(station);
        (LighterReport memory report,) = board.reportOf(station);
        uint48[] memory accounts = _accounts(master, report);
        calls = new Call[](accounts.length * (cancel ? 2 : 1));
        uint256 made;
        for (uint256 i = 0; i < accounts.length; i++) {
            calls[made++] = Call(
                address(exchange), 0, abi.encodeCall(IZkLighter.changePubKey, (accounts[i], KEY_SLOT, pubKey))
            );
            if (cancel) {
                calls[made++] =
                    Call(address(exchange), 0, abi.encodeCall(IZkLighter.cancelAllOrders, (accounts[i])));
            }
        }
    }

    function _withdrawCalls(address station, uint256 amount) private view returns (Call[] memory calls) {
        (uint48 master, bool exists) = masterOf(station);
        if (!exists) return new Call[](0);
        (LighterReport memory report, bool reported) = board.reportOf(station);
        if (!reported) {
            // Nothing is known of the account: only the executor's own ask.
            if (amount == type(uint256).max || amount == 0) return new Call[](0);
            calls = new Call[](1);
            calls[0] = _withdrawCall(master, amount);
            return calls;
        }
        uint48[] memory accounts = _accounts(master, report);
        if (amount == type(uint256).max) return _halves(station, report, accounts);
        calls = new Call[](accounts.length);
        uint256 made;
        uint256 left = amount;
        for (uint256 i = 0; i < accounts.length && left != 0; i++) {
            uint256 take = Math.min(left, _availableOf(report, accounts[i]));
            if (take == 0) continue;
            calls[made++] = _withdrawCall(accounts[i], take);
            left -= take;
        }
        assembly ("memory-safe") {
            mstore(calls, made)
        }
    }

    /// Everything still in the accounts, asked for without knowing what each
    /// holds now: by the newest post, the exchange's side less what is
    /// pending now is what is left in them (from a stale post, at least its
    /// equity), and each account is asked for
    /// that rounded up to a power of two, then each half below it down to a
    /// dollar. Carried out in order, each request at most what the account
    /// still holds goes through, so the account is emptied to under a dollar
    /// of whatever it holds up to twice the estimate, and no request ever
    /// relies on the exchange trimming one too large.
    function _halves(
        address station,
        LighterReport memory report,
        uint48[] memory accounts
    ) private view returns (Call[] memory calls) {
        int256 worth =
            _worth(station, report) - int256(uint256(exchange.getPendingBalance(station, DOLLAR_ASSET)));
        // A stale post (a stopped station nobody posts for) may say far less
        // than the closes since have left there: never ask for less than its
        // equity, so every pass drains up to twice that, again and again.
        if (!_fresh(report) && int256(report.equity) > worth) worth = int256(report.equity);
        if (worth < int256(_dollarUnit)) return new Call[](0);
        uint256 top = _dollarUnit;
        uint256 pieces = 1;
        while (top < uint256(worth) && top < type(uint64).max / 2) {
            top <<= 1;
            pieces++;
        }
        calls = new Call[](accounts.length * pieces);
        uint256 made;
        for (uint256 a = 0; a < accounts.length; a++) {
            for (uint256 piece = top; piece >= _dollarUnit; piece >>= 1) {
                calls[made++] = _withdrawCall(accounts[a], piece);
            }
        }
    }

    function _withdrawCall(uint48 account, uint256 amount) private view returns (Call memory) {
        if (amount > type(uint64).max) amount = type(uint64).max;
        return Call(
            address(exchange),
            0,
            abi.encodeCall(IZkLighter.withdraw, (account, DOLLAR_ASSET, PERPS, uint64(amount)))
        );
    }

    /// Every listed market of every account, both sides, whole:
    /// reduce-only, so only the side that closes something acts. With no
    /// band (zero), at any price; with one, no worse than it from the post's
    /// index, and a market the post has no price for is left to the full
    /// band.
    function _closeEverything(
        uint48 master,
        LighterReport memory report,
        uint16 bandBps
    ) private view returns (Call[] memory calls) {
        uint48[] memory accounts = _accounts(master, report);
        calls = new Call[](accounts.length * _markets.length * 2);
        uint256 made;
        for (uint256 a = 0; a < accounts.length; a++) {
            for (uint256 m = 0; m < _markets.length; m++) {
                uint256 sellAt = 1;
                uint256 buyAt = MAX_PRICE;
                if (bandBps != 0) {
                    uint32 index = _priceOf(report, _markets[m].index).index;
                    if (index == 0) continue;
                    sellAt = _limit(index, true, bandBps);
                    buyAt = _limit(index, false, bandBps);
                }
                calls[made++] = _order(accounts[a], _markets[m].index, 0, sellAt, true);
                calls[made++] = _order(accounts[a], _markets[m].index, 0, buyAt, false);
            }
        }
        assembly ("memory-safe") {
            mstore(calls, made)
        }
    }

    /// The worst price a closing order may take: `bandBps` from `index`,
    /// below it to close a long, above it to close a short.
    function _limit(uint256 index, bool long, uint16 bandBps) private pure returns (uint256 limit) {
        limit = long ? index.mulDiv(BPS - bandBps, BPS, Math.Rounding.Ceil) : index.mulDiv(BPS + bandBps, BPS);
        if (limit == 0) limit = 1;
        if (limit > MAX_PRICE) limit = MAX_PRICE;
    }

    function _order(
        uint48 account,
        uint16 market,
        uint256 size,
        uint256 limit,
        bool ask
    ) private view returns (Call memory) {
        return Call(
            address(exchange),
            0,
            abi.encodeCall(
                IZkLighter.createOrder,
                (account, market, uint48(size), uint32(limit), ask ? 1 : 0, MARKET_ORDER)
            )
        );
    }

    /// The master, then every other account the post names, once each.
    function _accounts(
        uint48 master,
        LighterReport memory report
    ) private pure returns (uint48[] memory list) {
        list = new uint48[](report.accounts.length + 1);
        list[0] = master;
        uint256 count = 1;
        for (uint256 i = 0; i < report.accounts.length; i++) {
            bool seen;
            for (uint256 j = 0; j < count; j++) {
                if (list[j] == report.accounts[i]) seen = true;
            }
            if (!seen) list[count++] = report.accounts[i];
        }
        assembly ("memory-safe") {
            mstore(list, count)
        }
        // The master last, so it is asked for whatever the others couldn't
        // give.
        if (count > 1) {
            list[0] = list[count - 1];
            list[count - 1] = master;
        }
    }

    function _availableOf(LighterReport memory report, uint48 account) private pure returns (uint256) {
        for (uint256 i = 0; i < report.accounts.length; i++) {
            if (report.accounts[i] == account) return report.available[i];
        }
        return 0;
    }

    function _fresh(LighterReport memory report) private view returns (bool) {
        return report.asOf <= block.timestamp && block.timestamp - report.asOf <= maxReportAge;
    }

    /// Whether any market held has its mark, bid or ask further from the
    /// index than the band, or isn't one the driver can value.
    function _outsideBand(LighterReport memory report, uint16 bandBps) private view returns (bool) {
        for (uint256 i = 0; i < report.positions.length; i++) {
            if (report.positions[i].size == 0) continue;
            (bool listed,) = _market(report.positions[i].market);
            if (!listed) return true;
            LighterPrice memory price = _priceOf(report, report.positions[i].market);
            if (price.index == 0) return true;
            uint256 low = uint256(price.index).mulDiv(BPS - bandBps, BPS, Math.Rounding.Ceil);
            uint256 high = uint256(price.index).mulDiv(BPS + bandBps, BPS);
            if (price.mark < low || price.mark > high) return true;
            if (price.bid < low || price.bid > high || price.ask < low || price.ask > high) return true;
        }
        return false;
    }

    function _priceOf(
        LighterReport memory report,
        uint16 market
    ) private pure returns (LighterPrice memory price) {
        for (uint256 i = 0; i < report.prices.length; i++) {
            if (report.prices[i].market == market) return report.prices[i];
        }
    }

    function _market(uint16 index) private view returns (bool, Market memory) {
        for (uint256 i = 0; i < _markets.length; i++) {
            if (_markets[i].index == index) return (true, _markets[i]);
        }
        Market memory none;
        return (false, none);
    }
}
