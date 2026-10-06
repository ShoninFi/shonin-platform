// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Call, Custody, ExitPath, IDriver, ValuationKind} from "../../interfaces/IDriver.sol";
import {IStation, StationSpec} from "../../interfaces/IStation.sol";
import {
    ConditionFlat,
    IAggregatorV3,
    ICustomCondition,
    IModuleProxyFactory,
    IRoles,
    ISafe,
    ISafeProxyFactory,
    IStockToken,
    ISwapRouter02,
    IUniswapV3Pool
} from "./RobinhoodChain.sol";
import {UniswapPrice} from "./UniswapPrice.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {IERC20Metadata} from "openzeppelin/token/ERC20/extensions/IERC20Metadata.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// Robinhood Chain's stock tokens, held in a Safe the station owns and
/// traded on Uniswap by the executor through a Zodiac Roles v2 role.
///
/// The station is the Safe's only owner, and the Roles module's owner. The
/// executor's key is a member of one role, which may make exactly two kinds
/// of call through the Safe: approve the router to spend a listed token, and
/// swap a listed stock token for the dollar or back, through its named pool,
/// paid to the Safe, for no less than the token's Chainlink price allows
/// within the station's exit band. So the key can trade the Safe's holdings
/// and can't send them anywhere else, buy anything else, or sell at a price
/// far from the feed.
///
/// Holdings are valued from Chainlink's per-token feeds, which already apply
/// the token's corporate-action multiplier: a balance is multiplied by the
/// feed's price and never by the multiplier again. The exit side is each
/// pool's time-weighted price over `twapWindow`, so a withdrawal is paid at
/// the lower of the feed and the market and a deposit pays the higher. A
/// feed is often hours old (it moves on a 0.5% change or a daily heartbeat,
/// and holds the last close while the cash market is shut), so its age alone
/// voids nothing: a market that has moved away from it further than the
/// band does, as does a feed older than `maxFeedAge`, a paused token oracle,
/// or a price that isn't positive.
///
/// It is immutable and has no owner. Its tokens, feeds and pools are fixed
/// at deployment; the role a station's Safe gets is scoped from them once,
/// when the Safe is set up, and never again.
contract RobinhoodDriver is IDriver, ICustomCondition {
    using Math for uint256;

    uint256 private constant WAD = 1e18;
    uint256 private constant BPS = 10_000;
    /// The role the executor's key is given.
    bytes32 public constant ROLE = "shonin-executor";
    /// Salt for the Safe's and the module's addresses, beside the station's
    /// own address in their initialisers.
    uint256 private constant SALT_NONCE = uint256(keccak256("shonin.robinhood.v1"));
    /// A holding worth less than this at the feed (a dollar) is dust: it is
    /// still sold in a close, at any price, but never holds a valuation off.
    uint256 private constant DUST_USD = 1e6;
    /// Below this many token units (a thousandth of a share) a holding whose
    /// feed can't be read is dust too.
    uint256 private constant DUST_UNITS = 1e15;

    // Roles v2's condition types and operators.
    uint8 private constant T_NONE = 0;
    uint8 private constant T_STATIC = 1;
    uint8 private constant T_TUPLE = 3;
    uint8 private constant T_CALLDATA = 5;
    uint8 private constant OP_PASS = 0;
    uint8 private constant OP_AND = 1;
    uint8 private constant OP_MATCHES = 5;
    uint8 private constant OP_EQUAL_TO_AVATAR = 15;
    uint8 private constant OP_EQUAL_TO = 16;
    uint8 private constant OP_CUSTOM = 22;

    struct Stock {
        address token;
        IAggregatorV3 feed;
        IUniswapV3Pool pool;
        uint24 fee;
        /// 10^(token decimals + feed decimals - dollar decimals).
        uint256 scale;
    }

    IERC20 public immutable dollar;
    address public immutable router;
    address public immutable safeSingleton;
    ISafeProxyFactory public immutable safeFactory;
    address public immutable rolesMastercopy;
    IModuleProxyFactory public immutable moduleFactory;
    /// Older than this, a feed voids a valuation.
    uint256 public immutable maxFeedAge;
    /// The window of each pool's time-weighted price.
    uint32 public immutable twapWindow;
    bytes32 private immutable _safeCodeHash;
    bytes32 private immutable _moduleCodeHash;

    Stock[] private _stocks;

    error NotListed();
    error Repeated();
    error BadPool();

    struct Venue {
        IERC20 dollar;
        address router;
        address safeSingleton;
        ISafeProxyFactory safeFactory;
        address rolesMastercopy;
        IModuleProxyFactory moduleFactory;
        uint256 maxFeedAge;
        uint32 twapWindow;
    }

    struct Listing {
        address token;
        IAggregatorV3 feed;
        IUniswapV3Pool pool;
    }

    constructor(Venue memory venue, Listing[] memory listings) {
        dollar = venue.dollar;
        router = venue.router;
        safeSingleton = venue.safeSingleton;
        safeFactory = venue.safeFactory;
        rolesMastercopy = venue.rolesMastercopy;
        moduleFactory = venue.moduleFactory;
        maxFeedAge = venue.maxFeedAge;
        twapWindow = venue.twapWindow;
        _safeCodeHash = keccak256(
            abi.encodePacked(venue.safeFactory.proxyCreationCode(), uint256(uint160(venue.safeSingleton)))
        );
        _moduleCodeHash = keccak256(
            abi.encodePacked(
                hex"602d8060093d393df3363d3d373d3d3d363d73",
                venue.rolesMastercopy,
                hex"5af43d82803e903d91602b57fd5bf3"
            )
        );
        uint8 dollarDecimals = IERC20Metadata(address(venue.dollar)).decimals();
        for (uint256 i = 0; i < listings.length; i++) {
            Listing memory listing = listings[i];
            if (listing.token == address(venue.dollar)) revert BadPool();
            for (uint256 j = 0; j < i; j++) {
                if (listings[j].token == listing.token) revert Repeated();
            }
            // The pool is the token against the dollar, and the one the
            // router reaches for that pair and fee.
            address token0 = listing.pool.token0();
            address token1 = listing.pool.token1();
            bool pair = (token0 == listing.token && token1 == address(venue.dollar))
                || (token1 == listing.token && token0 == address(venue.dollar));
            if (!pair) revert BadPool();
            uint8 decimals = IERC20Metadata(listing.token).decimals() + listing.feed.decimals();
            if (decimals < dollarDecimals) revert BadPool();
            _stocks.push(
                Stock({
                    token: listing.token,
                    feed: listing.feed,
                    pool: listing.pool,
                    fee: listing.pool.fee(),
                    scale: 10 ** (decimals - dollarDecimals)
                })
            );
        }
    }

    // What it is ------------------------------------------------------------

    function name() external pure returns (bytes32) {
        return "robinhood";
    }

    /// The router, both factories, and the calling station's own Safe and
    /// Roles module, whose addresses follow from the station's.
    function targets() external view returns (address[] memory list) {
        list = new address[](5);
        list[0] = router;
        list[1] = address(safeFactory);
        list[2] = address(moduleFactory);
        list[3] = safeOf(msg.sender);
        list[4] = rolesOf(msg.sender);
    }

    /// Who the stock tokens' terms exclude: US persons, and further people in
    /// Canada, Switzerland and the UK, and sanctioned persons. Sorted.
    function restricted() external pure returns (bytes32[] memory list) {
        list = new bytes32[](5);
        list[0] = "CA";
        list[1] = "CH";
        list[2] = "GB";
        list[3] = "SANCTIONED";
        list[4] = "US";
    }

    function custody() external pure returns (Custody) {
        return Custody.Station;
    }

    function exitPath() external pure returns (ExitPath) {
        return ExitPath.Contract;
    }

    function valuation() external pure returns (ValuationKind) {
        return ValuationKind.OnChain;
    }

    function stocks() external view returns (Stock[] memory) {
        return _stocks;
    }

    // Where a station's accounts are ---------------------------------------

    /// The station's Safe: one owner, the station, made by the Safe proxy
    /// factory from an initialiser that names only the station.
    function safeOf(address station) public view returns (address) {
        bytes32 salt = keccak256(abi.encodePacked(keccak256(_safeInit(station)), SALT_NONCE));
        return _create2(address(safeFactory), salt, _safeCodeHash);
    }

    /// The station's Roles module: owned by the station, acting for its Safe.
    function rolesOf(address station) public view returns (address) {
        bytes32 salt = keccak256(abi.encodePacked(keccak256(_rolesInit(station)), SALT_NONCE));
        return _create2(address(moduleFactory), salt, _moduleCodeHash);
    }

    /// Whether the station's Safe is set up: the module enabled, which is
    /// only ever done after the role is scoped.
    function isSetUp(address station) public view returns (bool) {
        address safe = safeOf(station);
        if (safe.code.length == 0) return false;
        return ISafe(safe).isModuleEnabled(rolesOf(station));
    }

    // Keys and money --------------------------------------------------------

    function approveKey(address station, address key, uint64) external view returns (Call[] memory calls) {
        Call[] memory setup = _setUp(station);
        calls = _append(setup, 1);
        bytes32[] memory roles = new bytes32[](1);
        roles[0] = ROLE;
        bool[] memory member = new bool[](1);
        member[0] = true;
        calls[setup.length] =
            Call(rolesOf(station), 0, abi.encodeCall(IRoles.assignRoles, (key, roles, member)));
    }

    function revokeKey(address station, address key) external view returns (Call[] memory calls) {
        address roles = rolesOf(station);
        if (roles.code.length == 0) return new Call[](0);
        bytes32[] memory keys = new bytes32[](1);
        keys[0] = ROLE;
        bool[] memory member = new bool[](1);
        calls = new Call[](1);
        calls[0] = Call(roles, 0, abi.encodeCall(IRoles.assignRoles, (key, keys, member)));
    }

    /// Into the Safe: the station approves its Safe, and the Safe pulls.
    function deposit(address station, uint256 amount) external view returns (Call[] memory calls) {
        Call[] memory setup = _setUp(station);
        calls = _append(setup, 2);
        address safe = safeOf(station);
        calls[setup.length] = Call(address(dollar), 0, abi.encodeCall(IERC20.approve, (safe, amount)));
        calls[setup.length + 1] =
            _exec(station, address(dollar), abi.encodeCall(IERC20.transferFrom, (station, safe, amount)));
    }

    function withdraw(address station, uint256 amount) external view returns (Call[] memory calls) {
        calls = new Call[](1);
        calls[0] = _exec(station, address(dollar), abi.encodeCall(IERC20.transfer, (station, amount)));
    }

    /// Sell `fraction` of every stock token the Safe holds for the dollar,
    /// each for no less than its feed price less the band, paid straight to
    /// the station, and send the station the same fraction of the Safe's
    /// idle dollars. So what reaches the station is exactly what the
    /// fraction fetched: a sale below the market's price falls on the
    /// leaver, never on the dollars the Safe keeps for everyone. Blocked
    /// when any holding worth more than dust has a feed that can't be
    /// trusted or a market further from its feed than the band, on either
    /// side. At the full band, everything is sold at any price.
    function close(
        address station,
        uint256 fraction,
        uint16 bandBps
    ) external view returns (Call[] memory calls, bool blocked) {
        address safe = safeOf(station);
        if (safe.code.length == 0) return (new Call[](0), false);
        uint256 count = _stocks.length;
        calls = new Call[](count * 2 + 1);
        uint256 made;
        for (uint256 i = 0; i < count; i++) {
            (Call[2] memory sale, bool sells, bool outside) =
                _sale(station, safe, _stocks[i], fraction, bandBps);
            if (outside) return (new Call[](0), true);
            if (!sells) continue;
            calls[made++] = sale[0];
            calls[made++] = sale[1];
        }
        uint256 idle = dollar.balanceOf(safe).mulDiv(fraction, WAD);
        if (idle != 0) {
            calls[made++] = _exec(station, address(dollar), abi.encodeCall(IERC20.transfer, (station, idle)));
        }
        assembly ("memory-safe") {
            mstore(calls, made)
        }
    }

    /// Nothing: a close pays the station itself, sale and idle dollars both,
    /// so a sweep has nothing left to bring. (Taking more from the Safe here
    /// would make the dollars everyone shares cover a leaver's bad fill.)
    function sweep(address, uint256) external pure returns (Call[] memory) {
        return new Call[](0);
    }

    /// The Safe's dollars and stock tokens: at the feeds for the mark, at
    /// each pool's time-weighted price for the exit side. A price that can't
    /// be read counts as nothing, and then `close` reports the market as
    /// outside the band, so no valuation uses the figure.
    function valueOf(address station) external view returns (uint256 mark, uint256 exit) {
        address safe = safeOf(station);
        if (safe.code.length == 0) return (0, 0);
        uint256 idle = dollar.balanceOf(safe);
        mark = idle;
        exit = idle;
        for (uint256 i = 0; i < _stocks.length; i++) {
            Stock memory stock = _stocks[i];
            uint256 balance = IERC20(stock.token).balanceOf(safe);
            if (balance == 0) continue;
            (, uint256 price) = _feedPrice(stock);
            mark += balance.mulDiv(price, stock.scale);
            exit += _marketValue(stock, balance);
        }
    }

    // The role's swap check -------------------------------------------------

    /// Roles v2 asks this of every swap the executor's key makes through
    /// the Safe: a listed stock against the dollar, through its named pool,
    /// for no less than the feed price allows within the tolerance the
    /// station's role was scoped with (its exit band, in `extra`).
    function check(
        address to,
        uint256 value,
        bytes calldata data,
        uint8 operation,
        uint256 location,
        uint256,
        bytes12 extra
    ) external view returns (bool, bytes32) {
        if (to != router || value != 0 || operation != 0) return (false, "target");
        ISwapRouter02.ExactInputSingleParams memory swap =
            abi.decode(data[location:location + 224], (ISwapRouter02.ExactInputSingleParams));
        uint256 tolerance = uint256(uint96(extra));
        if (tolerance > BPS) tolerance = BPS;
        for (uint256 i = 0; i < _stocks.length; i++) {
            Stock memory stock = _stocks[i];
            bool selling = swap.tokenIn == stock.token && swap.tokenOut == address(dollar);
            bool buying = swap.tokenIn == address(dollar) && swap.tokenOut == stock.token;
            if (!selling && !buying) continue;
            if (swap.fee != stock.fee) return (false, "pool");
            (bool trusted, uint256 price) = _feedPrice(stock);
            if (!trusted) return (false, "feed");
            uint256 fair =
                selling ? swap.amountIn.mulDiv(price, stock.scale) : swap.amountIn.mulDiv(stock.scale, price);
            if (swap.amountOutMinimum < fair.mulDiv(BPS - tolerance, BPS, Math.Rounding.Ceil)) {
                return (false, "price");
            }
            return (true, 0);
        }
        return (false, "token");
    }

    // Internals -------------------------------------------------------------

    /// One stock's part of a close: approve the router and sell the
    /// fraction, paid to the station.
    function _sale(
        address station,
        address safe,
        Stock memory stock,
        uint256 fraction,
        uint16 bandBps
    ) private view returns (Call[2] memory sale, bool sells, bool outside) {
        uint256 balance = IERC20(stock.token).balanceOf(safe);
        if (balance == 0) return (sale, false, false);
        (bool trusted, uint256 price) = _feedPrice(stock);
        bool dust = trusted ? balance.mulDiv(price, stock.scale) < DUST_USD : balance < DUST_UNITS;
        bool banded = bandBps < BPS && !dust;
        if (banded && (!trusted || _outsideBand(stock, price, bandBps))) return (sale, false, true);
        uint256 amountIn = balance.mulDiv(fraction, WAD, Math.Rounding.Ceil);
        if (amountIn > balance) amountIn = balance;
        if (amountIn == 0) return (sale, false, false);
        uint256 minOut = banded ? amountIn.mulDiv(price, stock.scale).mulDiv(BPS - bandBps, BPS) : 0;
        sale[0] = _exec(station, stock.token, abi.encodeCall(IERC20.approve, (router, amountIn)));
        sale[1] = _exec(
            station,
            router,
            abi.encodeCall(
                ISwapRouter02.exactInputSingle,
                (
                    ISwapRouter02.ExactInputSingleParams({
                        tokenIn: stock.token,
                        tokenOut: address(dollar),
                        fee: stock.fee,
                        recipient: station,
                        amountIn: amountIn,
                        amountOutMinimum: minOut,
                        sqrtPriceLimitX96: 0
                    })
                )
            )
        );
        sells = true;
    }

    /// The calls that set the station's Safe up, if it isn't yet: the Safe
    /// and the module made (unless someone already made them, with the
    /// same initialisers), the role scoped, then the module enabled.
    function _setUp(address station) private view returns (Call[] memory calls) {
        if (isSetUp(station)) return new Call[](0);
        address safe = safeOf(station);
        address roles = rolesOf(station);
        uint16 band = IStation(station).spec().exitBandBps;
        uint256 tokens = _stocks.length + 1;
        calls = new Call[](2 + 2 + tokens * 2 + 1);
        uint256 made;
        if (safe.code.length == 0) {
            calls[made++] = Call(
                address(safeFactory),
                0,
                abi.encodeCall(
                    ISafeProxyFactory.createProxyWithNonce, (safeSingleton, _safeInit(station), SALT_NONCE)
                )
            );
        }
        if (roles.code.length == 0) {
            calls[made++] = Call(
                address(moduleFactory),
                0,
                abi.encodeCall(
                    IModuleProxyFactory.deployModule, (rolesMastercopy, _rolesInit(station), SALT_NONCE)
                )
            );
        }
        calls[made++] = Call(roles, 0, abi.encodeCall(IRoles.scopeTarget, (ROLE, router)));
        calls[made++] = Call(
            roles,
            0,
            abi.encodeCall(
                IRoles.scopeFunction,
                (ROLE, router, ISwapRouter02.exactInputSingle.selector, _swapConditions(band), 0)
            )
        );
        for (uint256 i = 0; i < tokens; i++) {
            address token = i == 0 ? address(dollar) : _stocks[i - 1].token;
            calls[made++] = Call(roles, 0, abi.encodeCall(IRoles.scopeTarget, (ROLE, token)));
            calls[made++] = Call(
                roles,
                0,
                abi.encodeCall(
                    IRoles.scopeFunction, (ROLE, token, IERC20.approve.selector, _approveConditions(), 0)
                )
            );
        }
        calls[made++] = _exec(station, safe, abi.encodeCall(ISafe.enableModule, (roles)));
        assembly ("memory-safe") {
            mstore(calls, made)
        }
    }

    /// `exactInputSingle(params)`: the recipient is the Safe, and the whole
    /// swap passes `check` with the band as tolerance.
    function _swapConditions(uint16 band) private view returns (ConditionFlat[] memory c) {
        c = new ConditionFlat[](18);
        c[0] = ConditionFlat(0, T_CALLDATA, OP_MATCHES, "");
        c[1] = ConditionFlat(0, T_NONE, OP_AND, "");
        c[2] = ConditionFlat(1, T_TUPLE, OP_MATCHES, "");
        c[3] = ConditionFlat(1, T_TUPLE, OP_CUSTOM, abi.encodePacked(address(this), bytes12(uint96(band))));
        // The tuple's seven fields, checked: only the recipient.
        for (uint256 i = 0; i < 7; i++) {
            c[4 + i] = ConditionFlat(2, T_STATIC, i == 3 ? OP_EQUAL_TO_AVATAR : OP_PASS, "");
        }
        // The same seven, unchecked, giving the custom check its shape.
        for (uint256 i = 0; i < 7; i++) {
            c[11 + i] = ConditionFlat(3, T_STATIC, OP_PASS, "");
        }
    }

    /// `approve(spender, amount)`: only the router.
    function _approveConditions() private view returns (ConditionFlat[] memory c) {
        c = new ConditionFlat[](3);
        c[0] = ConditionFlat(0, T_CALLDATA, OP_MATCHES, "");
        c[1] = ConditionFlat(0, T_STATIC, OP_EQUAL_TO, abi.encode(router));
        c[2] = ConditionFlat(0, T_STATIC, OP_PASS, "");
    }

    /// The station's own call through its Safe: as the Safe's only owner and
    /// the caller, its signature is its address (Safe's "approved by the
    /// sender" form), so nothing is signed.
    function _exec(address station, address to, bytes memory data) private view returns (Call memory) {
        bytes memory signature = abi.encodePacked(uint256(uint160(station)), bytes32(0), uint8(1));
        return Call(
            safeOf(station),
            0,
            abi.encodeCall(
                ISafe.execTransaction, (to, 0, data, 0, 0, 0, 0, address(0), payable(address(0)), signature)
            )
        );
    }

    function _feedPrice(Stock memory stock) private view returns (bool trusted, uint256 price) {
        try stock.feed.latestRoundData() returns (uint80, int256 answer, uint256, uint256 updatedAt, uint80) {
            if (answer <= 0) return (false, 0);
            price = uint256(answer);
            trusted = updatedAt <= block.timestamp && block.timestamp - updatedAt <= maxFeedAge;
        } catch {
            return (false, 0);
        }
        // A token whose oracle the issuer has paused (a corporate action in
        // progress) is not priced by its feed meanwhile. A token without the
        // call (the testnet's) has nothing to pause.
        try IStockToken(stock.token).oraclePaused() returns (bool paused) {
            if (paused) trusted = false;
        } catch {}
    }

    function _marketValue(Stock memory stock, uint256 balance) private view returns (uint256) {
        try this.marketQuote(stock.pool, stock.token, balance) returns (uint256 quote) {
            return quote;
        } catch {
            return 0;
        }
    }

    /// What `amount` of `token` is worth in dollars at `pool`'s
    /// time-weighted price. External only so a pool that can't answer is
    /// caught.
    function marketQuote(
        IUniswapV3Pool pool,
        address token,
        uint256 amount
    ) external view returns (uint256) {
        int24 tick = UniswapPrice.meanTick(pool, twapWindow);
        return UniswapPrice.quoteAtTick(tick, amount, token, address(dollar));
    }

    function _outsideBand(Stock memory stock, uint256 price, uint16 bandBps) private view returns (bool) {
        // Compare a whole token's worth: feed against market.
        uint256 amount = 10 ** IERC20Metadata(stock.token).decimals();
        uint256 atFeed = amount.mulDiv(price, stock.scale);
        uint256 atMarket = _marketValue(stock, amount);
        if (atMarket == 0) return true;
        uint256 low = atFeed.mulDiv(BPS - bandBps, BPS, Math.Rounding.Ceil);
        uint256 high = atFeed.mulDiv(BPS + bandBps, BPS);
        return atMarket < low || atMarket > high;
    }

    function _safeInit(address station) private pure returns (bytes memory) {
        address[] memory owners = new address[](1);
        owners[0] = station;
        return abi.encodeCall(
            ISafe.setup, (owners, 1, address(0), "", address(0), address(0), 0, payable(address(0)))
        );
    }

    function _rolesInit(address station) private view returns (bytes memory) {
        address safe = safeOf(station);
        return abi.encodeCall(IRoles.setUp, (abi.encode(station, safe, safe)));
    }

    function _create2(address deployer, bytes32 salt, bytes32 codeHash) private pure returns (address) {
        return address(uint160(uint256(keccak256(abi.encodePacked(bytes1(0xff), deployer, salt, codeHash)))));
    }

    function _append(Call[] memory head, uint256 extra) private pure returns (Call[] memory calls) {
        calls = new Call[](head.length + extra);
        for (uint256 i = 0; i < head.length; i++) {
            calls[i] = head[i];
        }
    }
}
