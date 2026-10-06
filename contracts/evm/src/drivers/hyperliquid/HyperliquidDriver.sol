// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Call, Custody, ExitPath, IDelayedDriver, IDriver, ValuationKind} from "../../interfaces/IDriver.sol";
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
/// void and no leaver is paid at a figure nobody can check.
///
/// A holding worth less than the venue's smallest order can't be closed, so
/// it neither closes nor blocks.
///
/// Sending dollars home costs the account a little HYPE on HyperCore, so it
/// keeps some: up to `GAS_RESERVE` of it is the cost of sending home, not a
/// holding. It is never ordered in a close, never a reason to block, never
/// asked of a proof, and still counted in what the account is worth. With
/// less than `GAS_FLOOR` of it free, a partial close is blocked before
/// anything is sold, so a proved close is never stranded for want of a send.
/// The HYPE/USDC pair must be listed, so the driver can see it.
///
/// CoreWriter acts after the transaction and never reverts on a refusal, so
/// a close here is placed in one transaction and checked, against the
/// positions, in a later one (`ExitPath.Delayed`); the station sweeps and
/// delivers only after that check.
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
    /// The HYPE the account keeps for sends home, in hundredths of the
    /// token's wei: 1 HYPE is the cost of sending home, not a holding, and
    /// with less than 0.01 HYPE free a partial close is blocked. A send home
    /// cost 0.00002 HYPE on testnet.
    uint256 private constant GAS_RESERVE_HUNDREDTHS = 100;
    uint256 private constant GAS_FLOOR_HUNDREDTHS = 1;

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
    /// withdraws whichever wallet held it.
    function revokeKey(address station, address) external view returns (Call[] memory calls) {
        calls = new Call[](1);
        calls[0] = _write(HyperCore.addApiWallet(burnAgent(station), AGENT_NAME));
    }

    /// The address a station's agent name is handed to on a revoke: derived
    /// from a hash, so no key for it exists.
    function burnAgent(address station) public pure returns (address) {
        return address(uint160(uint256(keccak256(abi.encode("shonin-revoked-agent", station)))));
    }

    // Moving dollars --------------------------------------------------------

    /// Into the account's perp balance through Circle's deposit contract.
    function deposit(address, uint256 amount) external view returns (Call[] memory calls) {
        calls = new Call[](2);
        calls[0] = Call(address(usdc), 0, abi.encodeCall(IERC20.approve, (address(depositWallet), amount)));
        calls[1] = Call(
            address(depositWallet),
            0,
            abi.encodeCall(ICoreDepositWallet.deposit, (amount, HyperCore.MAIN_DEX))
        );
    }

    /// Back to the station on HyperEVM, as `sweep` does.
    function withdraw(address station, uint256 amount) external view returns (Call[] memory) {
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
    function close(
        address station,
        uint256 fraction,
        uint16 bandBps
    ) external view returns (Call[] memory calls, bool blocked) {
        if (_unlisted(station)) return (new Call[](0), true);
        // No HYPE to send the dollars home with: nothing is sold for a
        // leaver whose close could then never come home. A full unwind,
        // which everyone shares, goes ahead and sends once there is.
        if (fraction < WAD) {
            HyperCore.SpotBalance memory gas = HyperCore.spotBalance(station, _spots[_gas].token);
            if (uint256(gas.total) - uint256(gas.hold) < gasFloor) return (new Call[](0), true);
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

    /// The order closing `fraction` of one perp position, if any.
    function _perpClose(
        address station,
        Perp memory perp,
        uint256 fraction,
        uint16 bandBps
    ) private view returns (bytes memory action, bool outside) {
        int256 szi = HyperCore.position(station, perp.index).szi;
        if (szi == 0) return ("", false);
        bool isLong = szi > 0;
        uint256 open = uint256(isLong ? szi : -szi);
        uint256 size = _part(open, fraction);
        uint256 mark = HyperCore.markPx(perp.index);
        // A position the venue wouldn't take an order for whole is dust:
        // it can't be closed, and it can't block anything either.
        if (open * mark < MIN_ORDER) return ("", false);
        uint256 limit;
        {
            (uint256 bid, uint256 ask) = HyperCore.bbo(perp.index);
            (limit, outside) = _limit(isLong, HyperCore.oraclePx(perp.index), mark, bid, ask, bandBps);
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

    /// The order selling `fraction` of one spot holding, if any.
    function _spotClose(
        address station,
        uint256 index,
        uint256 fraction,
        uint16 bandBps
    ) private view returns (bytes memory action, bool outside) {
        Spot memory spot = _spots[index];
        HyperCore.SpotBalance memory balance = HyperCore.spotBalance(station, spot.token);
        uint256 lot = 10 ** (spot.weiDecimals - spot.szDecimals);
        // HYPE up to the reserve pays for sends home: only the rest is held.
        uint256 kept = index == _gas ? gasReserve : 0;
        uint256 total = uint256(balance.total);
        uint256 held = total > kept ? (total - kept) / lot : 0;
        if (held == 0) return ("", false);
        uint256 size = _part(held, fraction);
        // What open orders hold can't be sold until they are cancelled.
        uint256 spare = total - uint256(balance.hold);
        uint256 free = spare > kept ? (spare - kept) / lot : 0;
        if (size > free) size = free;
        uint256 venuePx = HyperCore.spotPx(spot.pair);
        // Dust.
        if (held * venuePx / 100 < MIN_ORDER) return ("", false);
        uint256 limit;
        {
            (uint256 bid, uint256 ask) = HyperCore.bbo(HyperCore.SPOT_ASSET_OFFSET + spot.pair);
            (limit, outside) = _limit(true, venuePx, venuePx, bid, ask, bandBps);
        }
        if (outside || size == 0 || size * venuePx / 100 < MIN_ORDER) return ("", outside);
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

    function closed(
        address station,
        int256[] calldata before,
        uint256 fraction
    ) external view returns (bool) {
        if (before.length != _perps.length + _spots.length) return false;
        int256[] memory now_ = exposure(station);
        for (uint256 i = 0; i < before.length; i++) {
            int256 was = before[i];
            // HYPE up to the reserve was never a holding, never ordered.
            if (i == _perps.length + _gas) {
                Spot memory gas = _spots[_gas];
                int256 kept = int256(gasReserve / 10 ** (gas.weiDecimals - gas.szDecimals));
                was = was > kept ? was - kept : int256(0);
                now_[i] = now_[i] > kept ? now_[i] - kept : int256(0);
            }
            if (was == 0) continue;
            bool isLong = was > 0;
            uint256 open = uint256(isLong ? was : -was);
            uint256 size = open.mulDiv(fraction, WAD, Math.Rounding.Ceil);
            if (size > open) size = open;
            // A piece too small to have been ordered for sure asks nothing.
            uint256 price = i < _perps.length
                ? HyperCore.markPx(_perps[i].index)
                : HyperCore.spotPx(_spots[i - _perps.length].pair) / 100;
            if (size * price < 2 * MIN_ORDER) continue;
            if (isLong ? now_[i] > was - int256(size) : now_[i] < was + int256(size)) {
                return false;
            }
        }
        return true;
    }

    /// The perp ledger (signed: what the perps' cash comes to after what
    /// was paid for them) and the spot dollars, in millionths.
    function ledger(address station) public view returns (int256) {
        return int256(HyperCore.marginSummary(station).rawUsd)
            + int256(uint256(HyperCore.spotBalance(station, USDC_TOKEN).total) / CORE_TO_EVM);
    }

    /// Each perp at its mark, each spot token at its spot price: the
    /// account's worth now, less the kept part of its worth then repriced at
    /// the same prices.
    function measure(
        address station,
        int256 dollars,
        int256[] calldata before,
        uint256 fraction
    ) external view returns (int256) {
        int256[] memory current = exposure(station);
        int256 then = dollars;
        int256 now_ = ledger(station);
        for (uint256 i = 0; i < current.length; i++) {
            int256 px = i < _perps.length
                ? int256(uint256(HyperCore.markPx(_perps[i].index)))
                : int256(uint256(HyperCore.spotPx(_spots[i - _perps.length].pair))) / 100;
            then += before[i] * px;
            now_ += current[i] * px;
        }
        return now_ - _keptPart(then, fraction);
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
    /// the lower of it and the bid. Money in the venue's own vaults is
    /// counted by the station, which knows where it is.
    function valueOf(address station) external view returns (uint256 mark, uint256 exit) {
        HyperCore.MarginSummary memory summary = HyperCore.marginSummary(station);
        int256 perpValue = summary.accountValue;
        uint256 discount;
        uint256 listed;
        for (uint256 i = 0; i < _perps.length; i++) {
            int256 szi = HyperCore.position(station, _perps[i].index).szi;
            if (szi == 0) continue;
            uint256 px = HyperCore.markPx(_perps[i].index);
            (uint256 bid, uint256 ask) = HyperCore.bbo(_perps[i].index);
            uint256 open = uint256(szi > 0 ? szi : -szi);
            listed += open * px;
            if (szi > 0) {
                uint256 exitPx = bid < px ? bid : px;
                discount += open * (px - exitPx);
            } else {
                uint256 exitPx = ask > px ? ask : px;
                discount += open * (exitPx - px);
            }
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
            Spot memory spot = _spots[i];
            uint256 total = HyperCore.spotBalance(station, spot.token).total;
            if (total == 0) continue;
            uint256 px = HyperCore.spotPx(spot.pair);
            (uint256 bid,) = HyperCore.bbo(HyperCore.SPOT_ASSET_OFFSET + spot.pair);
            uint256 scale = 10 ** (uint256(spot.weiDecimals) + 8 - spot.szDecimals);
            mark += total.mulDiv(px * 1e6, scale);
            exit += total.mulDiv((bid < px ? bid : px) * 1e6, scale);
        }
    }

    /// Whether the account holds a perp the driver doesn't list: its total
    /// notional is more than the listed perps' (with a dollar-unit of
    /// rounding for each).
    function _unlisted(address station) private view returns (bool) {
        uint256 total = HyperCore.marginSummary(station).ntlPos;
        if (total == 0) return false;
        uint256 listed;
        for (uint256 i = 0; i < _perps.length; i++) {
            int256 szi = HyperCore.position(station, _perps[i].index).szi;
            if (szi == 0) continue;
            listed += uint256(szi > 0 ? szi : -szi) * HyperCore.markPx(_perps[i].index);
        }
        return total > listed + _perps.length + 1;
    }

    function _write(bytes memory action) private pure returns (Call memory) {
        return Call(HyperCore.CORE_WRITER, 0, abi.encodeCall(ICoreWriter.sendRawAction, (action)));
    }
}
