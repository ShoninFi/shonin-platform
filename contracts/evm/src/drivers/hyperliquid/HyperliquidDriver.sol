// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Call, Custody, ExitPath, IDelayedDriver, IDriver, ValuationKind} from "../../interfaces/IDriver.sol";
import {IStation} from "../../interfaces/IStation.sol";
import {CloseBand} from "../CloseBand.sol";
import {DelayedVenue} from "../DelayedVenue.sol";
import {HyperCore, ICoreDepositWallet, ICoreWriter} from "./HyperCore.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

import {Strings} from "openzeppelin/utils/Strings.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// Hyperliquid's driver on HyperEVM: the station is the master of its own
/// HyperCore account, acting through CoreWriter and valued from the read
/// precompiles.
///
/// It is immutable and has no owner. What it trades is fixed when it is
/// deployed: a list of the venue's own perps and of spot pairs against the
/// dollar, with their decimals read from the venue then. A station holding a
/// perp outside the list can't be valued at its exit side, so the driver
/// reports every market as outside the band until it is gone: valuations are
/// void and no leaver is paid at a figure nobody can check. A listed market
/// the venue stops answering for is treated the same way. A full unwind is
/// never held by either: it closes every listed market it can read, and
/// leaves the rest, worth nothing to it.
///
/// A holding worth less than the venue's smallest order can't be closed, so
/// it neither closes nor blocks; a spot holding like that can't be sold
/// either, so it is worth nothing here: it can't hold a halted vault open,
/// and nobody can give the account a little of a token to do so.
///
/// Sending dollars home costs the account a little HYPE on HyperCore, so it
/// keeps some: up to `GAS_RESERVE` of it is the cost of sending home, not a
/// holding. It is never ordered in a close, never a reason to block, never
/// asked of a proof, and worth nothing in the account's value or a close's
/// measure, since nothing ever sells it. With
/// less than `GAS_FLOOR` of it free, a partial close is blocked before
/// anything is sold, so a proved close is never stranded for want of a send.
/// The HYPE/USDC pair must be listed, so the driver can see it.
///
/// CoreWriter acts after the transaction and never reverts on a refusal, so
/// a close here is placed in one transaction and checked, against the
/// positions, in a later one (`ExitPath.Delayed`); the station sweeps and
/// delivers only after that check, and only once `SETTLE_DELAY` has passed,
/// so the close's own orders have run by then.
///
/// HyperCore credits a deposit from HyperEVM after the block it was made
/// in, and a send home lands on HyperEVM a block after HyperCore runs it, so
/// for `CREDIT_WINDOW` after dollars move between the station and the
/// account they may show in neither: valuations are void and no close is
/// staged meanwhile, and the executor's moves are spaced so that a priced
/// valuation always fits between two of them.
///
/// A claim's close is refused before anything is sold when the account
/// couldn't release the fraction's worth after it: what stays, the rest of
/// the account's worth, must cover the venue's transfer requirement on what
/// stays open, with a margin. The close's slippage is the leaver's, taken
/// off what is asked of the venue, so it never has to fit in what stays.
contract HyperliquidDriver is IDriver, IDelayedDriver {
    using Math for uint256;

    uint256 private constant WAD = 1e18;
    uint256 private constant BPS = 10_000;
    /// The dollar on HyperCore, and where a spot send of it goes to reach
    /// the same address on HyperEVM.
    uint64 private constant USDC_TOKEN = 0;
    address private constant USDC_SYSTEM = 0x2000000000000000000000000000000000000000;
    /// HyperCore keeps the dollar in hundred-millionths, HyperEVM in
    /// millionths.
    uint256 private constant CORE_TO_EVM = 100;
    /// The venue refuses an order worth less than ten dollars, without
    /// reverting. A close of a smaller piece is not ordered; the post-check
    /// asks nothing of a piece worth less than twice that, so a price that
    /// moved between the two can't leave a claim stuck.
    uint256 private constant MIN_ORDER = 10e6;
    /// The venue refuses an order priced further than this from its own
    /// price; a close at any price stays just inside it.
    uint256 private constant FULL_BAND_FLOOR_BPS = 2500;
    uint256 private constant FULL_BAND_CEILING_BPS = 17_500;
    /// The named agent the station approves. Approving another wallet under
    /// the same name replaces it, which is how one executor at a time holds
    /// the account.
    string public constant AGENT_NAME = "shonin";
    /// How the station keeps its balances: spot and perp apart, as class
    /// transfers and the valuation here assume. Setting it pins it, so an
    /// agent can't change it later.
    uint8 public constant ABSTRACTION = 1;
    /// Agents are approved for at most this long; the venue's own limit.
    uint64 private constant MAX_AGENT_LIFE = 180 days;
    /// A staged close fills within a second of its block and its dollars
    /// reach HyperEVM a block after the sweep: five minutes is ample for
    /// either, and a move between the station and the account lands within
    /// it too.
    uint64 public constant DELIVERY_WINDOW = 5 minutes;
    /// CoreWriter's orders and transfers run "a few seconds" after their
    /// block, by the venue's documentation; a minute is ample.
    uint64 public constant SETTLE_DELAY = 1 minutes;
    /// A deposit from HyperEVM is credited right after its EVM block, and a
    /// send home is queued until the next EVM block: dollars moved are
    /// in neither place for a block or two. A minute is ample.
    uint64 public constant CREDIT_WINDOW = 1 minutes;
    /// The venue lets dollars leave the perp side only while what stays
    /// covers the larger of its initial margin and a tenth of what stays
    /// open; a claim's close also leaves a tenth more than that.
    uint256 private constant TRANSFER_BPS = 1000;
    uint256 private constant SAFETY_BPS = 1000;
    /// The HYPE the account keeps for sends home, in hundredths of the
    /// token's wei: 1 HYPE is the cost of sending home, not a holding, and
    /// with less than 0.01 HYPE free a partial close is blocked. A send home
    /// cost 0.00002 HYPE on testnet.
    uint256 private constant GAS_RESERVE_HUNDREDTHS = 100;
    uint256 private constant GAS_FLOOR_HUNDREDTHS = 1;
    /// What a taker fill pays the venue at most, in basis points (its base
    /// tier: 0.045% on a perp, 0.07% on spot), counted against a close's
    /// least.
    uint256 private constant FEE_BOUND_BPS = 10;

    IERC20 public immutable usdc;
    ICoreDepositWallet public immutable depositWallet;

    struct Perp {
        uint32 index;
        uint8 szDecimals;
    }

    struct Spot {
        uint32 pair;
        uint64 token;
        uint8 szDecimals;
        uint8 weiDecimals;
    }

    Perp[] private _perps;
    Spot[] private _spots;
    /// Which of `_spots` is HYPE, the token sends home are paid in.
    uint256 private immutable _gas;
    /// HYPE kept for sends home, and the least free for a partial close, in
    /// its wei.
    uint256 public immutable gasReserve;
    uint256 public immutable gasFloor;

    error NotTheDollar();
    error Repeated();
    error NoGasToken();

    /// `perps` are the venue's perp indices, `spotPairs` its spot pair
    /// indices; each pair must be quoted in the dollar. Both read from the
    /// venue, which numbers them differently on each network.
    constructor(
        IERC20 usdc_,
        ICoreDepositWallet depositWallet_,
        uint32[] memory perps,
        uint32[] memory spotPairs
    ) {
        usdc = usdc_;
        depositWallet = depositWallet_;
        for (uint256 i = 0; i < perps.length; i++) {
            for (uint256 j = 0; j < i; j++) {
                if (perps[j] == perps[i]) revert Repeated();
            }
            _perps.push(Perp({index: perps[i], szDecimals: HyperCore.perpAssetInfo(perps[i]).szDecimals}));
        }
        uint256 gas = type(uint256).max;
        for (uint256 i = 0; i < spotPairs.length; i++) {
            for (uint256 j = 0; j < i; j++) {
                if (spotPairs[j] == spotPairs[i]) revert Repeated();
            }
            HyperCore.SpotInfo memory info = HyperCore.spotInfo(spotPairs[i]);
            if (info.tokens[1] != USDC_TOKEN || info.tokens[0] == USDC_TOKEN) revert NotTheDollar();
            HyperCore.TokenInfo memory token = HyperCore.tokenInfo(info.tokens[0]);
            if (keccak256(bytes(token.name)) == keccak256("HYPE")) gas = i;
            _spots.push(
                Spot({
                    pair: spotPairs[i],
                    token: info.tokens[0],
                    szDecimals: token.szDecimals,
                    weiDecimals: token.weiDecimals
                })
            );
        }
        if (gas == type(uint256).max) revert NoGasToken();
        _gas = gas;
        uint256 hundredth = 10 ** _spots[gas].weiDecimals / 100;
        gasReserve = GAS_RESERVE_HUNDREDTHS * hundredth;
        gasFloor = GAS_FLOOR_HUNDREDTHS * hundredth;
    }

    // What it is ------------------------------------------------------------

    function name() external pure returns (bytes32) {
        return "hyperliquid";
    }

    function targets() external view returns (address[] memory list) {
        list = new address[](2);
        list[0] = HyperCore.CORE_WRITER;
        list[1] = address(depositWallet);
    }

    /// Who the venue's terms exclude: US persons, residents of Ontario, and
    /// sanctioned persons and places. Sorted.
    function restricted() external pure returns (bytes32[] memory list) {
        list = new bytes32[](3);
        list[0] = "CA-ON";
        list[1] = "SANCTIONED";
        list[2] = "US";
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

    /// Trusted except within `CREDIT_WINDOW` of a move of dollars between
    /// the station and the account.
    function priced(address station) public view returns (bool) {
        return block.timestamp >= uint256(_movedAt(station)) + CREDIT_WINDOW;
    }

    /// A whole slot after the credit window of the last move.
    function movableAt(address station) public view returns (uint256) {
        uint256 moved = _movedAt(station);
        return DelayedVenue.slotAfter(station, moved == 0 ? 0 : moved + CREDIT_WINDOW);
    }

    function _movedAt(address station) private view returns (uint64) {
        return IStation(station).movedAt(DelayedVenue.nameIn(station, address(this)));
    }

    function valuation() external pure returns (ValuationKind) {
        return ValuationKind.OnChain;
    }

    function perps() external view returns (Perp[] memory) {
        return _perps;
    }

    function spots() external view returns (Spot[] memory) {
        return _spots;
    }

    // Keys ------------------------------------------------------------------

    /// Pin the account's balance mode, then approve `key` as the named agent
    /// until `validUntil` (capped at the venue's limit). Both are queued, so
    /// on an account HyperCore doesn't know yet both are dropped; asking
    /// again once it is activated does it.
    function approveKey(
        address station,
        address key,
        uint64 validUntil
    ) external view returns (Call[] memory calls) {
        uint64 cap = uint64(block.timestamp) + MAX_AGENT_LIFE;
        if (validUntil > cap) validUntil = cap;
        calls = new Call[](2);
        calls[0] = _write(HyperCore.setAbstraction(station, ABSTRACTION));
        calls[1] = _write(
            HyperCore.addApiWallet(
                key, string.concat(AGENT_NAME, " valid_until ", Strings.toString(uint256(validUntil) * 1000))
            )
        );
    }

    /// Hand the agent's name to an address nobody holds a key for, which
    /// withdraws whichever wallet held it. The account has one agent name,
    /// so that is done only for the key the station has in place: any other
    /// key was already replaced, and naming it withdraws nothing.
    function revokeKey(address station, address key) external view returns (Call[] memory calls) {
        if (key != IStation(station).approvedKey(DelayedVenue.nameIn(station, address(this)))) {
            return new Call[](0);
        }
        calls = new Call[](1);
        calls[0] = _write(HyperCore.addApiWallet(burnAgent(station), AGENT_NAME));
    }

    /// The address a station's agent name is handed to on a revoke: derived
    /// from a hash, so no key for it exists.
    function burnAgent(address station) public pure returns (address) {
        return address(uint160(uint256(keccak256(abi.encode("shonin-revoked-agent", station)))));
    }

    // Moving dollars --------------------------------------------------------

    /// Into the account's perp balance through Circle's deposit contract;
    /// never sooner than the spacing after the last move (`DelayedVenue`).
    function deposit(address station, uint256 amount) external view returns (Call[] memory calls) {
        DelayedVenue.spaced(movableAt(station));
        calls = new Call[](2);
        calls[0] = Call(address(usdc), 0, abi.encodeCall(IERC20.approve, (address(depositWallet), amount)));
        calls[1] = Call(
            address(depositWallet),
            0,
            abi.encodeCall(ICoreDepositWallet.deposit, (amount, HyperCore.MAIN_DEX))
        );
    }

    /// Back to the station on HyperEVM, as `sweep` does; spaced as a
    /// deposit is.
    function withdraw(address station, uint256 amount) external view returns (Call[] memory) {
        DelayedVenue.spaced(movableAt(station));
        return _home(station, amount);
    }

    /// The account's free dollars home, up to `amount` (millionths): the spot
    /// balance first, then what the perp side can spare, moved to spot; then
    /// one send to the dollar's system address. The send costs the account a
    /// little HYPE on HyperCore, which it must keep there. It lands on
    /// HyperEVM a block later.
    function sweep(address station, uint256 amount) external view returns (Call[] memory) {
        return _home(station, amount);
    }

    function _home(address station, uint256 amount) private view returns (Call[] memory calls) {
        HyperCore.SpotBalance memory spot = HyperCore.spotBalance(station, USDC_TOKEN);
        uint256 spotFree = (uint256(spot.total) - uint256(spot.hold)) / CORE_TO_EVM;
        uint256 perpFree = HyperCore.withdrawable(station);
        uint256 take = amount.min(spotFree + perpFree);
        if (take == 0) return new Call[](0);
        uint256 fromPerp = take > spotFree ? take - spotFree : 0;
        calls = new Call[](fromPerp == 0 ? 1 : 2);
        uint256 n;
        if (fromPerp != 0) calls[n++] = _write(HyperCore.usdClassTransfer(uint64(fromPerp), false));
        calls[n] = _write(HyperCore.spotSend(USDC_SYSTEM, USDC_TOKEN, uint64(take * CORE_TO_EVM)));
    }

    // Closing ---------------------------------------------------------------

    /// Reduce-only immediate-or-cancel orders for `fraction` of every
    /// position, each limited to the venue's own price (the oracle for a
    /// perp, the spot price for a pair) less or plus the band, and only
    /// while every price of every market held is inside the band.
    ///
    /// At the full band nothing blocks: the listed markets are closed at any
    /// price, and what the driver can't close is left where it is, worth
    /// nothing to the close: a perp it doesn't list, and a listed market the
    /// venue doesn't answer for. Inside the band either of those blocks.
    function close(
        address station,
        uint256 fraction,
        uint16 bandBps
    ) external view returns (Call[] memory calls, bool blocked) {
        // A stopped station's unwind is closed as any close at its band:
        // this driver keeps no schedule of its own past the full band.
        bandBps = CloseBand.band(bandBps);
        bool full = bandBps >= BPS;
        if (!full && _unlisted(station)) return (new Call[](0), true);
        // No HYPE to send the dollars home with: nothing is sold for a
        // leaver whose close could then never come home. A full unwind,
        // which everyone shares, goes ahead and sends once there is.
        // Nor while dollars moved may be in neither place (the record a
        // stage keeps would miss them), nor when what the close frees
        // couldn't leave the account.
        if (fraction < WAD) {
            (bool read, uint64 gasTotal, uint64 gasHold) =
                HyperCore.trySpotBalance(station, _spots[_gas].token);
            if (!read || uint256(gasTotal) - uint256(gasHold) < gasFloor) return (new Call[](0), true);
            if (!priced(station) || !_releasable(station, fraction)) return (new Call[](0), true);
        }
        calls = new Call[](_perps.length + _spots.length);
        uint256 count;
        for (uint256 i = 0; i < _perps.length + _spots.length; i++) {
            (bytes memory action, bool outside) = i < _perps.length
                ? _perpClose(station, _perps[i], fraction, bandBps)
                : _spotClose(station, i - _perps.length, fraction, bandBps);
            if (outside) return (new Call[](0), true);
            if (action.length != 0) calls[count++] = _write(action);
        }
        assembly ("memory-safe") {
            mstore(calls, count)
        }
    }

    /// Whether the perp side could release `fraction` of its worth after a
    /// close of `fraction`: with `v` its worth, `n` its open notional and
    /// `r` the venue's transfer requirement, the larger of the initial
    /// margin used and `TRANSFER_BPS` of `n`, what stays after the leaver's
    /// part is taken, `v - f v`, must cover `(1 - f) r` grossed up by
    /// `SAFETY_BPS`. What the close's orders lose to slippage is the
    /// leaver's: the measure takes it off what is asked of the venue, so it
    /// never comes out of what stays. Spot dollars and tokens sold for them
    /// are always free.
    function _releasable(address station, uint256 fraction) private view returns (bool) {
        HyperCore.MarginSummary memory summary = HyperCore.marginSummary(station);
        uint256 ntl = summary.ntlPos;
        if (ntl == 0) return true;
        if (summary.accountValue <= 0) return false;
        uint256 worth = uint256(int256(summary.accountValue));
        uint256 required = Math.max(summary.marginUsed, ntl.mulDiv(TRANSFER_BPS, BPS, Math.Rounding.Ceil));
        uint256 kept = required.mulDiv(WAD - fraction, WAD, Math.Rounding.Ceil).mulDiv(
            BPS + SAFETY_BPS, BPS, Math.Rounding.Ceil
        );
        return worth >= kept + worth.mulDiv(fraction, WAD, Math.Rounding.Ceil);
    }

    /// The order closing `fraction` of one perp position, if any. A market
    /// whose reads fail is outside the band, and at the full band is left.
    function _perpClose(
        address station,
        Perp memory perp,
        uint256 fraction,
        uint16 bandBps
    ) private view returns (bytes memory action, bool outside) {
        bool full = bandBps >= BPS;
        (bool read, int64 szi) = HyperCore.tryPosition(station, perp.index);
        if (!read) return ("", !full);
        if (szi == 0) return ("", false);
        bool isLong = szi > 0;
        uint256 open = uint256(int256(isLong ? szi : -szi));
        uint256 size = _part(open, fraction);
        uint256 mark;
        uint256 venuePx;
        {
            uint64 m;
            uint64 o;
            (read, m) = HyperCore.tryMarkPx(perp.index);
            if (!read) return ("", !full);
            mark = m;
            // A position the venue wouldn't take an order for whole is dust:
            // it can't be closed, and it can't block anything either.
            if (open * mark < MIN_ORDER) return ("", false);
            (read, o) = HyperCore.tryOraclePx(perp.index);
            // No price to limit an order by is no answer either.
            if (!read || o == 0) return ("", !full);
            venuePx = o;
        }
        uint256 limit;
        {
            // At the full band the book doesn't matter.
            uint64 bid;
            uint64 ask;
            if (!full) {
                (read, bid, ask) = HyperCore.tryBbo(perp.index);
                if (!read) return ("", true);
            }
            (limit, outside) = _limit(isLong, venuePx, mark, bid, ask, bandBps);
        }
        // Size times a perp price is already millionths of a dollar.
        if (outside || size * mark < MIN_ORDER) return ("", outside);
        action = HyperCore.limitOrder(
            perp.index,
            !isLong,
            uint64(limit * 10 ** (2 + perp.szDecimals)),
            uint64(size * 10 ** (8 - perp.szDecimals)),
            true,
            HyperCore.TIF_IOC
        );
    }

    /// The order selling `fraction` of one spot holding, if any. A market
    /// whose reads fail is outside the band, and at the full band is left.
    function _spotClose(
        address station,
        uint256 index,
        uint256 fraction,
        uint16 bandBps
    ) private view returns (bytes memory action, bool outside) {
        bool full = bandBps >= BPS;
        Spot memory spot = _spots[index];
        (bool read, uint64 balanceTotal, uint64 balanceHold) = HyperCore.trySpotBalance(station, spot.token);
        if (!read) return ("", !full);
        uint256 lot = 10 ** (spot.weiDecimals - spot.szDecimals);
        // HYPE up to the reserve pays for sends home: only the rest is held.
        uint256 kept = index == _gas ? gasReserve : 0;
        uint256 total = uint256(balanceTotal);
        uint256 held = total > kept ? (total - kept) / lot : 0;
        if (held == 0) return ("", false);
        uint256 size = _part(held, fraction);
        // What open orders hold can't be sold until they are cancelled.
        uint256 spare = total - uint256(balanceHold);
        uint256 free = spare > kept ? (spare - kept) / lot : 0;
        uint256 venuePx;
        {
            uint64 px;
            (read, px) = HyperCore.trySpotPx(spot.pair);
            if (!read || px == 0) return ("", !full);
            venuePx = px;
        }
        // Dust.
        if (held * venuePx / 100 < MIN_ORDER) return ("", false);
        uint256 limit;
        {
            uint64 bid;
            uint64 ask;
            if (!full) {
                (read, bid, ask) = HyperCore.tryBbo(HyperCore.SPOT_ASSET_OFFSET + spot.pair);
                if (!read) return ("", true);
            }
            (limit, outside) = _limit(true, venuePx, venuePx, bid, ask, bandBps);
        }
        if (outside) return ("", true);
        if (size > free) {
            // A claim's close that would order this piece, and can't order
            // all of it, sells nothing anywhere (reported blocked): the proof
            // would ask for the piece, and the other markets' sales would be
            // the leaver's to pay for with nothing to show. A full unwind,
            // which everyone shares, sells what it can.
            if (fraction < WAD && size * venuePx / 100 >= MIN_ORDER) return ("", true);
            size = free;
        }
        if (size == 0 || size * venuePx / 100 < MIN_ORDER) return ("", false);
        action = HyperCore.limitOrder(
            HyperCore.SPOT_ASSET_OFFSET + spot.pair,
            false,
            uint64(limit * 10 ** spot.szDecimals),
            uint64(size * 10 ** (8 - spot.szDecimals)),
            false,
            HyperCore.TIF_IOC
        );
    }

    /// `fraction` of `open` lots, rounded up, never more than all of it.
    function _part(uint256 open, uint256 fraction) private pure returns (uint256 size) {
        size = open.mulDiv(fraction, WAD, Math.Rounding.Ceil);
        if (size > open) size = open;
    }

    /// The worst price a closing order may take, rounded to what the venue
    /// accepts and toward the stricter side, and whether a market is outside
    /// the band: its closing side, or any of its mark, bid and ask, further
    /// from the venue's own price than the band, on either side. At the full
    /// band any price is accepted.
    function _limit(
        bool isLong,
        uint256 venuePx,
        uint256 mark,
        uint256 bid,
        uint256 ask,
        uint16 bandBps
    ) private pure returns (uint256 limit, bool outside) {
        if (venuePx == 0) return (0, true);
        if (bandBps >= BPS) {
            limit = isLong
                ? _sigFigs(venuePx.mulDiv(FULL_BAND_FLOOR_BPS, BPS, Math.Rounding.Ceil), true)
                : _sigFigs(venuePx.mulDiv(FULL_BAND_CEILING_BPS, BPS), false);
            return (limit, false);
        }
        uint256 low = venuePx.mulDiv(BPS - bandBps, BPS, Math.Rounding.Ceil);
        uint256 high = venuePx.mulDiv(BPS + bandBps, BPS);
        // An empty side is as far from the price as a side can be.
        if (bid == 0 || ask == 0) return (0, true);
        if (mark < low || mark > high || bid < low || bid > high || ask < low || ask > high) return (0, true);
        limit = isLong ? _sigFigs(low, true) : _sigFigs(high, false);
    }

    /// The venue takes at most five significant figures: round up for a
    /// floor, down for a ceiling, so the band is never exceeded.
    function _sigFigs(uint256 price, bool up) private pure returns (uint256) {
        uint256 unit = 1;
        while (price / unit >= 100_000) unit *= 10;
        if (unit == 1) return price;
        uint256 rounded = (price / unit) * unit;
        if (up && rounded < price) rounded += unit;
        return rounded;
    }

    // Proving a close -------------------------------------------------------

    function exposure(address station) public view returns (int256[] memory sizes) {
        sizes = new int256[](_perps.length + _spots.length);
        for (uint256 i = 0; i < _perps.length; i++) {
            sizes[i] = HyperCore.position(station, _perps[i].index).szi;
        }
        for (uint256 i = 0; i < _spots.length; i++) {
            Spot memory spot = _spots[i];
            sizes[_perps.length + i] = int256(
                uint256(HyperCore.spotBalance(station, spot.token).total)
                    / 10 ** (spot.weiDecimals - spot.szDecimals)
            );
        }
    }

    /// The exposure, then each market's own price as the close's orders are
    /// limited by it (a perp's oracle, a pair's spot price), in hundredths of
    /// a millionth of a dollar per lot, as `measure` counts.
    function snapshot(address station) external view returns (int256[] memory held) {
        int256[] memory sizes = exposure(station);
        uint256 n = sizes.length;
        held = new int256[](2 * n);
        for (uint256 i = 0; i < n; i++) {
            held[i] = sizes[i];
            held[n + i] = int256(
                i < _perps.length
                    ? uint256(HyperCore.oraclePx(_perps[i].index)) * 100
                    : HyperCore.spotPx(_spots[i - _perps.length].pair)
            );
        }
    }

    /// Not before `since`, when the close's own orders have run: then, for
    /// each market it ordered (a piece too small to have been ordered for
    /// sure asks nothing), the share of what it ordered that the position is
    /// smaller by, on the same side, rounded down; the smallest of them.
    ///
    /// The venue's reads can't tell her fills from a reduction the venue
    /// forces (a liquidation, a deleveraging): only the position's size is
    /// read. So a position smaller by more than her order there (the size
    /// the close ordered, in the venue's own lots), on the other side, or,
    /// where she ordered, larger, holds something that isn't hers, and
    /// nothing is known: the stage can't be measured, and lapses with
    /// nothing paid.
    function closedPart(
        address station,
        int256[] calldata before,
        uint256 fraction,
        uint64 since
    ) external view returns (uint256 part, bool known) {
        if (block.timestamp < since) return (0, false);
        uint256 n = _perps.length + _spots.length;
        if (before.length != 2 * n) return (0, true);
        int256[] memory now_ = exposure(station);
        part = WAD;
        for (uint256 i = 0; i < n; i++) {
            int256 was = before[i];
            int256 is_ = now_[i];
            // HYPE up to the reserve was never a holding, never ordered.
            if (i == _perps.length + _gas) (was, is_) = (_overReserve(was), _overReserve(is_));
            if (was == 0) continue;
            bool isLong = was > 0;
            uint256 open = uint256(isLong ? was : -was);
            uint256 size = _part(open, fraction);
            // Crossed, or smaller by more than her order could fill: not
            // hers, whether or not this piece was ordered.
            int256 done = isLong ? was - is_ : is_ - was;
            if ((isLong ? is_ < 0 : is_ > 0) || done > int256(size)) return (0, false);
            // Millionths of a dollar, times a hundred.
            uint256 price = i < _perps.length
                ? uint256(HyperCore.markPx(_perps[i].index)) * 100
                : HyperCore.spotPx(_spots[i - _perps.length].pair);
            if (size * price < 2 * MIN_ORDER * 100) continue;
            // Larger than before: not hers either.
            if (done < 0) return (0, false);
            // Her order missed: nothing of it closed, and the rest is still
            // read for anything that isn't hers.
            uint256 share = uint256(done) >= size ? WAD : uint256(done).mulDiv(WAD, size);
            if (share < part) part = share;
        }
        known = true;
    }

    /// Lots of HYPE above the reserve.
    function _overReserve(int256 lots) private view returns (int256) {
        Spot memory gas = _spots[_gas];
        int256 kept = int256(gasReserve / 10 ** (gas.weiDecimals - gas.szDecimals));
        return lots > kept ? lots - kept : int256(0);
    }

    /// The perp ledger (signed: what the perps' cash comes to after what
    /// was paid for them) and the spot dollars, in millionths.
    function ledger(address station) public view returns (int256) {
        return int256(HyperCore.marginSummary(station).rawUsd)
            + int256(uint256(HyperCore.spotBalance(station, USDC_TOKEN).total) / CORE_TO_EVM);
    }

    /// Each perp at its mark, each spot token at its spot price (HYPE up to
    /// the reserve, and a holding worth less than the smallest order, at
    /// nothing then, and as much of it at nothing now): the account's worth
    /// now, less the rest of its worth then (1 - `fraction` x `part`,
    /// rounded up) repriced at the same prices; and the least the close
    /// fetched, her part of the account then with every order filled at its
    /// limit from the stage's own prices, less what a fill pays the venue,
    /// never below `bandBps` of what the close ordered. Figures are kept in hundredths of a millionth until the
    /// end, which rounds down.
    function measure(
        address station,
        int256 dollars,
        int256[] calldata before,
        uint256 fraction,
        uint256 part,
        uint16 bandBps
    ) external view returns (int256 measured, int256 least) {
        int256[] memory current = exposure(station);
        uint256 n = current.length;
        int256 then = dollars * 100;
        int256 now_ = ledger(station) * 100;
        // The account as it was, every order filled at its limit.
        int256 worst = dollars * 100;
        uint256 ordered;
        uint256 orderedThen;
        uint256 band = Math.min(bandBps, BPS);
        for (uint256 i = 0; i < n; i++) {
            int256 was = before[i];
            int256 is_ = current[i];
            uint256 px;
            if (i < _perps.length) {
                px = uint256(HyperCore.markPx(_perps[i].index)) * 100;
            } else {
                // What the account held then and didn't count (HYPE up to
                // the reserve, a holding worth less than the smallest
                // order) stays uncounted now, and nothing else: whatever it
                // holds above that now counts at the price, however small
                // (the rest of a holding the close sold part of, HYPE
                // bought since with dollars the ledger no longer has), and
                // whatever it holds less than that (HYPE spent on a send)
                // is never the leaver's.
                px = HyperCore.spotPx(_spots[i - _perps.length].pair);
                int256 counted = _sellable(i == _perps.length + _gas ? _overReserve(was) : was, px);
                int256 uncounted = was - counted;
                was = counted;
                is_ = is_ > uncounted ? is_ - uncounted : int256(0);
            }
            then += was * int256(px);
            now_ += is_ * int256(px);
            ordered += _part(uint256(was < 0 ? -was : was), fraction) * px;
            // A long sold no lower than the stage's own price less the band,
            // a short bought back no higher than it plus the band: the
            // limits her orders were placed at, less what a fill pays the
            // venue. Only the stage's prices: her own orders can't have
            // filled worse than their limits.
            uint256 stagePx = uint256(before[n + i]);
            orderedThen += _part(uint256(was < 0 ? -was : was), fraction) * stagePx;
            worst += was > 0
                ? was * int256(stagePx.mulDiv(BPS - band - FEE_BOUND_BPS, BPS))
                : was * int256(stagePx.mulDiv(BPS + band + FEE_BOUND_BPS, BPS, Math.Rounding.Ceil));
        }
        uint256 share = fraction.mulDiv(part, WAD);
        measured = now_ - _keptPart(then, share);
        least = -int256(ordered.mulDiv(band, BPS, Math.Rounding.Ceil));
        // Her part of the account as it was with every order of the part
        // closed filled at its limit: the least her close fetched. A
        // measure below it is a loss her orders could not have made (a
        // reduction the venue forced, funding, an order resting before the
        // stage that filled before the executor cancelled it), which the
        // whole pool shares: the station lapses the stage rather than pay
        // or charge it. A price that moved after her orders filled changes
        // neither figure: her part was sold, and the rest is repriced on
        // both sides.
        // What wasn't closed of what was ordered may have filled and been
        // traded back since: no more than the band on it at the stage's
        // prices is the leaver's to lose.
        int256 floor = _share(worst, share)
            - int256(
                orderedThen.mulDiv(band, BPS, Math.Rounding.Ceil).mulDiv(WAD - part, WAD, Math.Rounding.Ceil)
            );
        if (floor > least) least = floor;
        // Down, toward minus infinity.
        measured = measured >= 0 ? measured / 100 : -((-measured + 99) / 100);
        least = least >= 0 ? least / 100 : -((-least + 99) / 100);
    }

    /// Spot lots worth less than the smallest order at `px` (a hundredth of
    /// a millionth per lot): nothing can sell them.
    function _sellable(int256 lots, uint256 px) private pure returns (int256) {
        return lots > 0 && uint256(lots) * px < MIN_ORDER * 100 ? int256(0) : lots;
    }

    /// `fraction` of `worth`, rounded down (toward minus infinity).
    function _share(int256 worth, uint256 fraction) private pure returns (int256) {
        uint256 size = uint256(worth < 0 ? -worth : worth);
        uint256 part = size.mulDiv(fraction, WAD, worth < 0 ? Math.Rounding.Ceil : Math.Rounding.Floor);
        return worth < 0 ? -int256(part) : int256(part);
    }

    /// (1 - `fraction`) of `worth`, rounded up.
    function _keptPart(int256 worth, uint256 fraction) private pure returns (int256) {
        uint256 size = uint256(worth < 0 ? -worth : worth);
        uint256 part = size.mulDiv(WAD - fraction, WAD, worth < 0 ? Math.Rounding.Floor : Math.Rounding.Ceil);
        return worth < 0 ? -int256(part) : int256(part);
    }

    // Venue vaults ----------------------------------------------------------

    function heldValue(address station, address[] calldata held) external view returns (uint256 total) {
        for (uint256 i = 0; i < held.length; i++) {
            (uint64 equity,) = HyperCore.vaultEquity(station, held[i]);
            total += equity;
        }
    }

    function reclaim(
        address station,
        address held
    ) external view returns (Call[] memory calls, uint256 equity) {
        uint64 lockedUntil;
        (equity, lockedUntil) = HyperCore.vaultEquity(station, held);
        if (equity == 0 || uint256(lockedUntil) > block.timestamp * 1000) return (new Call[](0), equity);
        calls = new Call[](1);
        calls[0] = _write(HyperCore.vaultTransfer(held, false, uint64(equity)));
    }

    // Valuing ---------------------------------------------------------------

    /// The account at the venue's marks, and at its exit side: each long at
    /// the lower of its mark and bid, each short at the higher of its mark
    /// and ask. Spot dollars count at par, spot tokens at the spot price or
    /// the lower of it and the bid; HYPE up to the reserve, and a token
    /// holding worth less than the smallest order, at nothing, since nothing
    /// sells them. Money in the venue's own vaults is counted by the
    /// station, which knows where it is.
    ///
    /// A market whose reads fail is never counted above what is known: a
    /// perp's whole notional comes off the exit side, as a perp the driver
    /// doesn't list does, and a spot holding counts as nothing (at the exit
    /// side only, where only its book can't be read). `close` reports such a
    /// market as outside the band, so no valuation prices on these figures.
    function valueOf(address station) external view returns (uint256 mark, uint256 exit) {
        HyperCore.MarginSummary memory summary = HyperCore.marginSummary(station);
        int256 perpValue = summary.accountValue;
        uint256 discount;
        uint256 listed;
        for (uint256 i = 0; i < _perps.length; i++) {
            (bool known, uint256 open, uint256 off) = _perpDiscount(station, _perps[i].index);
            if (!known) continue;
            listed += open;
            discount += off;
        }
        // A position the driver doesn't know is worth nothing to a leaver.
        if (summary.ntlPos > listed + _perps.length + 1) discount += summary.ntlPos - listed;
        if (perpValue > 0) {
            mark = uint256(perpValue);
            exit = mark > discount ? mark - discount : 0;
        }
        uint256 dollars = HyperCore.spotBalance(station, USDC_TOKEN).total / CORE_TO_EVM;
        mark += dollars;
        exit += dollars;
        for (uint256 i = 0; i < _spots.length; i++) {
            (uint256 m, uint256 x) = _spotWorth(station, i);
            mark += m;
            exit += x;
        }
    }

    /// One listed perp's notional at its mark, and what selling it at the
    /// exit side would lose against the mark; `known` is false when any of
    /// its reads fails.
    function _perpDiscount(
        address station,
        uint32 index
    ) private view returns (bool known, uint256 notional, uint256 off) {
        (bool read, int64 szi) = HyperCore.tryPosition(station, index);
        if (!read) return (false, 0, 0);
        if (szi == 0) return (true, 0, 0);
        uint64 m;
        uint64 bid;
        uint64 ask;
        (read, m) = HyperCore.tryMarkPx(index);
        if (!read) return (false, 0, 0);
        (read, bid, ask) = HyperCore.tryBbo(index);
        if (!read) return (false, 0, 0);
        uint256 px = m;
        uint256 open = uint256(int256(szi > 0 ? szi : -szi));
        notional = open * px;
        if (szi > 0) {
            uint256 exitPx = bid < px ? bid : px;
            off = open * (px - exitPx);
        } else {
            uint256 exitPx = ask > px ? ask : px;
            off = open * (exitPx - px);
        }
        known = true;
    }

    /// One listed spot holding at its price and at the lower of it and the
    /// bid; nothing for what can't be read.
    function _spotWorth(address station, uint256 i) private view returns (uint256 mark, uint256 exit) {
        Spot memory spot = _spots[i];
        (bool read, uint64 balance,) = HyperCore.trySpotBalance(station, spot.token);
        if (!read) return (0, 0);
        uint256 total = balance;
        if (i == _gas) total = total > gasReserve ? total - gasReserve : 0;
        if (total == 0) return (0, 0);
        uint64 px;
        (read, px) = HyperCore.trySpotPx(spot.pair);
        if (!read) return (0, 0);
        uint256 scale = 10 ** (uint256(spot.weiDecimals) + 8 - spot.szDecimals);
        mark = total.mulDiv(uint256(px) * 1e6, scale);
        if (mark < MIN_ORDER) return (0, 0);
        uint64 bid;
        (read, bid,) = HyperCore.tryBbo(HyperCore.SPOT_ASSET_OFFSET + spot.pair);
        if (!read) return (mark, 0);
        exit = total.mulDiv((bid < px ? bid : px) * uint256(1e6), scale);
    }

    /// Whether the account holds a perp the driver doesn't list: its total
    /// notional is more than the listed perps' (with a dollar-unit of
    /// rounding for each). A listed perp whose reads fail counts as one it
    /// doesn't list.
    function _unlisted(address station) private view returns (bool) {
        uint256 total = HyperCore.marginSummary(station).ntlPos;
        if (total == 0) return false;
        uint256 listed;
        for (uint256 i = 0; i < _perps.length; i++) {
            (bool read, int64 szi) = HyperCore.tryPosition(station, _perps[i].index);
            if (!read) return true;
            if (szi == 0) continue;
            uint64 px;
            (read, px) = HyperCore.tryMarkPx(_perps[i].index);
            if (!read) return true;
            listed += uint256(int256(szi > 0 ? szi : -szi)) * px;
        }
        return total > listed + _perps.length + 1;
    }

    function _write(bytes memory action) private pure returns (Call memory) {
        return Call(HyperCore.CORE_WRITER, 0, abi.encodeCall(ICoreWriter.sendRawAction, (action)));
    }
}
