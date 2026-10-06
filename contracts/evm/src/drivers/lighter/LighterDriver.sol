// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Call, Custody, ExitPath, IDelayedDriver, IDriver, ValuationKind} from "../../interfaces/IDriver.sol";
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
/// or book sits further from the exchange's index than the band.
///
/// The exchange carries out the station's requests after the transaction, so
/// a claim's close is staged (`ExitPath.Delayed`): reduce-only orders are
/// placed, a later post proves the positions shrank, then a withdrawal is
/// requested and, minutes later, paid to the station.
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
        return "lighter";
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
    function deposit(address station, uint256 amount) external view returns (Call[] memory calls) {
        calls = new Call[](2);
        calls[0] = Call(address(dollar), 0, abi.encodeCall(IERC20.approve, (address(exchange), amount)));
        calls[1] = Call(
            address(exchange), 0, abi.encodeCall(IZkLighter.deposit, (station, DOLLAR_ASSET, PERPS, amount))
        );
    }

    /// Ask for `amount` from the accounts (the master's first), and pay out
    /// whatever earlier requests left pending.
    function withdraw(address station, uint256 amount) external view returns (Call[] memory) {
        return _withdrawCalls(station, amount);
    }

    /// The same: after a close, the freed dollars are asked for, and land as
    /// a pending balance minutes later, which anyone may then pay out.
    function sweep(address station, uint256 amount) external view returns (Call[] memory) {
        return _withdrawCalls(station, amount);
    }

    // Closing ---------------------------------------------------------------

    /// Reduce-only orders closing `fraction` of every position, each no worse
    /// than the band from the exchange's index. Blocked when the newest post
    /// is too old or any held market is outside the band. At the full band,
    /// every listed market of every account is closed whole at any price,
    /// needing no post: a reduce-only order of zero size is the whole
    /// position, and the side that holds nothing does nothing.
    function close(
        address station,
        uint256 fraction,
        uint16 bandBps
    ) external view returns (Call[] memory calls, bool blocked) {
        (uint48 master, bool exists) = masterOf(station);
        if (!exists) return (new Call[](0), false);
        (LighterReport memory report, bool reported) = board.reportOf(station);
        if (bandBps >= BPS) return (_closeEverything(master, report), false);
        if (!reported || !_fresh(report)) return (new Call[](0), true);
        if (_outsideBand(report, bandBps)) return (new Call[](0), true);

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
            LighterPrice memory price = _priceOf(report, position.market);
            uint256 limit = long
                ? uint256(price.index).mulDiv(BPS - bandBps, BPS, Math.Rounding.Ceil)
                : uint256(price.index).mulDiv(BPS + bandBps, BPS);
            if (limit == 0) limit = 1;
            if (limit > MAX_PRICE) limit = MAX_PRICE;
            calls[made++] = Call(
                address(exchange),
                0,
                abi.encodeCall(
                    IZkLighter.createOrder,
                    (
                        position.account,
                        position.market,
                        uint48(size),
                        uint32(limit),
                        long ? 1 : 0,
                        MARKET_ORDER
                    )
                )
            );
        }
        assembly ("memory-safe") {
            mstore(calls, made)
        }
    }

    /// The accounts' worth from the newest post (however old: a stale post
    /// voids the valuation through `close`), plus what withdrawals have left
    /// pending for the station on-chain. The exit side takes each position
    /// at the book's closing side where that is worse than the mark.
    ///
    /// The pending balance counts at the lower of what the post saw and what
    /// is pending now: a withdrawal carried out since the post is still in
    /// the post's equity, and a balance paid out since the post is in the
    /// station's own dollars, so neither is counted twice.
    function valueOf(address station) external view returns (uint256 mark, uint256 exit) {
        uint256 pending = exchange.getPendingBalance(station, DOLLAR_ASSET);
        (LighterReport memory report, bool reported) = board.reportOf(station);
        if (!reported) return (pending, pending);
        if (report.pending < pending) pending = report.pending;
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
        mark = report.equity + pending;
        exit = (report.equity > haircut ? report.equity - haircut : 0) + pending;
    }

    // Delayed closes --------------------------------------------------------

    /// Each listed market's net position over all the station's accounts, in
    /// the driver's market order, then the post's time: what `closed`
    /// compares a later post against.
    function exposure(address station) external view returns (int256[] memory held) {
        (LighterReport memory report,) = board.reportOf(station);
        held = new int256[](_markets.length + 1);
        for (uint256 i = 0; i < report.positions.length; i++) {
            for (uint256 m = 0; m < _markets.length; m++) {
                if (_markets[m].index == report.positions[i].market) held[m] += report.positions[i].size;
            }
        }
        held[_markets.length] = int256(uint256(report.asOf));
    }

    /// Whether a post newer than the one `before` came from shows every
    /// market at least `fraction` smaller, on the same side or flat.
    function closed(
        address station,
        int256[] calldata before,
        uint256 fraction
    ) external view returns (bool) {
        if (before.length != _markets.length + 1) return false;
        int256[] memory now_ = this.exposure(station);
        if (now_[_markets.length] <= before[_markets.length]) return false;
        for (uint256 m = 0; m < _markets.length; m++) {
            int256 was = before[m];
            int256 is_ = now_[m];
            if (was == 0) continue;
            if ((was > 0 && is_ < 0) || (was < 0 && is_ > 0)) return false;
            uint256 open = uint256(was > 0 ? was : -was);
            uint256 left = uint256(is_ >= 0 ? is_ : -is_);
            uint256 wanted = open - open.mulDiv(fraction, WAD, Math.Rounding.Ceil);
            if (left > wanted) return false;
        }
        return true;
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
        uint256 pending = exchange.getPendingBalance(station, DOLLAR_ASSET);
        if (!exists) return new Call[](0);
        (LighterReport memory report, bool reported) = board.reportOf(station);
        uint48[] memory accounts = _accounts(master, report);
        calls = new Call[](accounts.length + 1);
        uint256 made;
        if (pending != 0) {
            calls[made++] = Call(
                address(exchange),
                0,
                abi.encodeCall(IZkLighter.withdrawPendingBalance, (station, DOLLAR_ASSET, uint128(pending)))
            );
        }
        uint256 left = amount;
        for (uint256 i = 0; i < accounts.length && left != 0; i++) {
            uint256 take = left;
            // What each account can give, where a post says; the master is
            // asked for the rest when nothing is known.
            if (reported) {
                uint256 available = _availableOf(report, accounts[i]);
                if (take > available) take = available;
                if (i == accounts.length - 1 && accounts[i] == master) take = left;
            }
            if (take == 0) continue;
            if (take > type(uint64).max) take = type(uint64).max;
            calls[made++] = Call(
                address(exchange),
                0,
                abi.encodeCall(IZkLighter.withdraw, (accounts[i], DOLLAR_ASSET, PERPS, uint64(take)))
            );
            left -= take;
        }
        assembly ("memory-safe") {
            mstore(calls, made)
        }
    }

    /// Every listed market of every account, both sides, whole, at any
    /// price: reduce-only, so only the side that closes something acts.
    function _closeEverything(
        uint48 master,
        LighterReport memory report
    ) private view returns (Call[] memory calls) {
        uint48[] memory accounts = _accounts(master, report);
        calls = new Call[](accounts.length * _markets.length * 2);
        uint256 made;
        for (uint256 a = 0; a < accounts.length; a++) {
            for (uint256 m = 0; m < _markets.length; m++) {
                calls[made++] = Call(
                    address(exchange),
                    0,
                    abi.encodeCall(
                        IZkLighter.createOrder, (accounts[a], _markets[m].index, 0, 1, 1, MARKET_ORDER)
                    )
                );
                calls[made++] = Call(
                    address(exchange),
                    0,
                    abi.encodeCall(
                        IZkLighter.createOrder,
                        (accounts[a], _markets[m].index, 0, MAX_PRICE, 0, MARKET_ORDER)
                    )
                );
            }
        }
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
