// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {LocalUsd} from "../../src/LocalUsd.sol";
import {HyperCore} from "../../src/drivers/hyperliquid/HyperCore.sol";
import {Vm} from "forge-std/Vm.sol";

/// HyperCore as far as a station sees it, for tests: the read precompiles
/// answer from this contract's state, and CoreWriter queues each action,
/// which does nothing until `process` runs the queue, as the venue does after
/// the block. Like the venue, it drops an action it can't do instead of
/// reverting.
///
/// Units follow the venue: a perp price is the human price times
/// 10^(6 - szDecimals), a position size counts lots of 10^-szDecimals, so
/// size times price is millionths of a dollar; spot balances are in each
/// token's own wei, the dollar's in hundred-millionths.
contract MockHyperCore {
    Vm private constant VM = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    uint256 private constant BPS = 10_000;
    address private constant USDC_SYSTEM = 0x2000000000000000000000000000000000000000;

    struct Market {
        uint64 mark;
        uint64 oracle;
        uint64 bid;
        uint64 ask;
        uint8 szDecimals;
        bool listed;
    }

    struct SpotPair {
        uint64 base;
        uint64 px;
        uint64 bid;
        uint64 ask;
    }

    struct Token {
        uint8 szDecimals;
        uint8 weiDecimals;
    }

    struct Queued {
        address sender;
        bytes data;
    }

    LocalUsd public immutable usd;
    /// Margin each perp position needs, in basis points of its notional.
    uint256 public marginBps = 1000;
    /// Orders fill no more than this many lots each; zero for no limit.
    uint256 public depth;

    mapping(uint32 => Market) public perp;
    uint32[] public perpList;
    mapping(uint32 => SpotPair) public pair;
    mapping(uint64 => Token) public token;

    mapping(address => bool) public exists;
    mapping(address => int256) public rawUsd;
    mapping(address => mapping(uint32 => int64)) public szi;
    mapping(address => mapping(uint64 => uint64)) public spotTotal;
    mapping(address => mapping(uint64 => uint64)) public spotHold;
    mapping(address => mapping(address => uint64)) public vaultEquity;
    mapping(address => mapping(address => uint64)) public vaultLockedUntil;
    mapping(address => mapping(bytes32 => address)) public agentNamed;
    mapping(address => string) public lastAgentName;
    mapping(address => uint8) public abstraction;

    Queued[] private _queue;
    uint256 public dropped;

    constructor(LocalUsd usd_) {
        usd = usd_;
        token[0] = Token({szDecimals: 8, weiDecimals: 8});
        PrecompileStub stub = new PrecompileStub(this);
        address[13] memory precompiles = [
            HyperCore.POSITION,
            HyperCore.SPOT_BALANCE,
            HyperCore.VAULT_EQUITY,
            HyperCore.WITHDRAWABLE,
            HyperCore.MARK_PX,
            HyperCore.ORACLE_PX,
            HyperCore.SPOT_PX,
            HyperCore.PERP_ASSET_INFO,
            HyperCore.SPOT_INFO,
            HyperCore.TOKEN_INFO,
            HyperCore.BBO,
            HyperCore.ACCOUNT_MARGIN_SUMMARY,
            HyperCore.CORE_USER_EXISTS
        ];
        for (uint256 i = 0; i < precompiles.length; i++) {
            VM.etch(precompiles[i], address(stub).code);
        }
        VM.etch(HyperCore.CORE_WRITER, address(new CoreWriterStub(this)).code);
    }

    // Setting the scene -----------------------------------------------------

    function listPerp(uint32 index, uint8 szDecimals, uint64 price) external {
        perp[index] = Market(price, price, price, price, szDecimals, true);
        perpList.push(index);
    }

    function setPerp(uint32 index, uint64 mark, uint64 oracle, uint64 bid, uint64 ask) external {
        Market storage m = perp[index];
        (m.mark, m.oracle, m.bid, m.ask) = (mark, oracle, bid, ask);
    }

    function listSpot(
        uint32 index,
        uint64 base,
        uint8 szDecimals,
        uint8 weiDecimals,
        uint64 price
    ) external {
        pair[index] = SpotPair(base, price, price, price);
        token[base] = Token(szDecimals, weiDecimals);
    }

    function setSpot(uint32 index, uint64 px, uint64 bid, uint64 ask) external {
        SpotPair storage p = pair[index];
        (p.px, p.bid, p.ask) = (px, bid, ask);
    }

    function setDepth(uint256 lots) external {
        depth = lots;
    }

    /// A first transfer to an account brings it into being.
    function credit(address user, int256 perpDollars) external {
        exists[user] = true;
        rawUsd[user] += perpDollars;
    }

    function creditSpot(address user, uint64 tokenIndex, uint64 amount) external {
        exists[user] = true;
        spotTotal[user][tokenIndex] += amount;
    }

    /// The account's agent trading: a fill at `px`.
    function trade(address user, uint32 index, int64 lots, uint64 px) external {
        szi[user][index] += lots;
        rawUsd[user] -= int256(lots) * int256(uint256(px));
    }

    /// The account's agent parking money in a venue vault.
    function parkInVault(address user, address vault, uint64 amount, uint64 lockedUntilMs) external {
        rawUsd[user] -= int256(uint256(amount));
        vaultEquity[user][vault] += amount;
        vaultLockedUntil[user][vault] = lockedUntilMs;
    }

    // Reads -----------------------------------------------------------------

    function accountValue(address user) public view returns (int256 value, uint256 ntl) {
        value = rawUsd[user];
        for (uint256 i = 0; i < perpList.length; i++) {
            int256 size = szi[user][perpList[i]];
            if (size == 0) continue;
            int256 mark = int256(uint256(perp[perpList[i]].mark));
            value += size * mark;
            ntl += uint256(size > 0 ? size * mark : -size * mark);
        }
    }

    function withdrawableOf(address user) public view returns (uint256) {
        (int256 value, uint256 ntl) = accountValue(user);
        int256 free = value - int256((ntl * marginBps) / BPS);
        // What the perp side holds in cash bounds it too.
        if (free > rawUsd[user]) free = rawUsd[user];
        return free > 0 ? uint256(free) : 0;
    }

    function answer(address precompile, bytes calldata input) external view returns (bytes memory) {
        if (precompile == HyperCore.POSITION) {
            (address user, uint16 index) = abi.decode(input, (address, uint16));
            return abi.encode(HyperCore.Position(szi[user][index], 0, 0, 0, false));
        }
        if (precompile == HyperCore.SPOT_BALANCE) {
            (address user, uint64 t) = abi.decode(input, (address, uint64));
            return abi.encode(HyperCore.SpotBalance(spotTotal[user][t], spotHold[user][t], 0));
        }
        if (precompile == HyperCore.VAULT_EQUITY) {
            (address user, address vault) = abi.decode(input, (address, address));
            return abi.encode(vaultEquity[user][vault], vaultLockedUntil[user][vault]);
        }
        if (precompile == HyperCore.WITHDRAWABLE) {
            address user = abi.decode(input, (address));
            return abi.encode(uint64(withdrawableOf(user)));
        }
        if (precompile == HyperCore.MARK_PX) return abi.encode(perp[abi.decode(input, (uint32))].mark);
        if (precompile == HyperCore.ORACLE_PX) return abi.encode(perp[abi.decode(input, (uint32))].oracle);
        if (precompile == HyperCore.SPOT_PX) return abi.encode(pair[abi.decode(input, (uint32))].px);
        if (precompile == HyperCore.PERP_ASSET_INFO) {
            Market memory m = perp[abi.decode(input, (uint32))];
            require(m.listed, "no such perp");
            return abi.encode(HyperCore.PerpAssetInfo("PERP", 0, m.szDecimals, 50, false));
        }
        if (precompile == HyperCore.SPOT_INFO) {
            SpotPair memory p = pair[abi.decode(input, (uint32))];
            return abi.encode(HyperCore.SpotInfo("PAIR", [p.base, uint64(0)]));
        }
        if (precompile == HyperCore.TOKEN_INFO) {
            Token memory t = token[abi.decode(input, (uint64))];
            return abi.encode(
                HyperCore.TokenInfo(
                    "TOKEN", new uint64[](0), 0, address(0), address(0), t.szDecimals, t.weiDecimals, 0
                )
            );
        }
        if (precompile == HyperCore.BBO) {
            uint32 asset = abi.decode(input, (uint32));
            if (asset >= HyperCore.SPOT_ASSET_OFFSET) {
                SpotPair memory p = pair[asset - HyperCore.SPOT_ASSET_OFFSET];
                return abi.encode(p.bid, p.ask);
            }
            return abi.encode(perp[asset].bid, perp[asset].ask);
        }
        if (precompile == HyperCore.ACCOUNT_MARGIN_SUMMARY) {
            (, address user) = abi.decode(input, (uint32, address));
            (int256 value, uint256 ntl) = accountValue(user);
            return abi.encode(
                HyperCore.MarginSummary(
                    int64(value), uint64((ntl * marginBps) / BPS), uint64(ntl), int64(rawUsd[user])
                )
            );
        }
        if (precompile == HyperCore.CORE_USER_EXISTS) return abi.encode(exists[abi.decode(input, (address))]);
        revert("unknown precompile");
    }

    // CoreWriter ------------------------------------------------------------

    function enqueue(address sender, bytes calldata data) external {
        _queue.push(Queued(sender, data));
    }

    function queued() external view returns (uint256) {
        return _queue.length;
    }

    function queuedAt(uint256 i) external view returns (address, bytes memory) {
        return (_queue[i].sender, _queue[i].data);
    }

    /// Run every queued action, in order, as the venue does after a block.
    function process() external {
        for (uint256 i = 0; i < _queue.length; i++) {
            if (!_apply(_queue[i].sender, _queue[i].data)) dropped += 1;
        }
        delete _queue;
    }

    function _apply(address sender, bytes memory data) private returns (bool) {
        if (!exists[sender] || data.length < 4 || uint8(data[0]) != 1) return false;
        uint24 id = uint24(uint8(data[1])) << 16 | uint24(uint8(data[2])) << 8 | uint24(uint8(data[3]));
        bytes memory fields = new bytes(data.length - 4);
        for (uint256 i = 0; i < fields.length; i++) {
            fields[i] = data[i + 4];
        }
        if (id == HyperCore.LIMIT_ORDER) return _order(sender, fields);
        if (id == HyperCore.VAULT_TRANSFER) {
            (address vault, bool isDeposit, uint64 usdAmount) = abi.decode(fields, (address, bool, uint64));
            if (isDeposit || vaultLockedUntil[sender][vault] > block.timestamp * 1000) return false;
            if (vaultEquity[sender][vault] < usdAmount) return false;
            vaultEquity[sender][vault] -= usdAmount;
            rawUsd[sender] += int256(uint256(usdAmount));
            return true;
        }
        if (id == HyperCore.SPOT_SEND) {
            (address destination, uint64 t, uint64 amount) = abi.decode(fields, (address, uint64, uint64));
            if (spotTotal[sender][t] - spotHold[sender][t] < amount) return false;
            spotTotal[sender][t] -= amount;
            if (destination == USDC_SYSTEM && t == 0) {
                usd.mint(sender, amount / 100);
            } else {
                exists[destination] = true;
                spotTotal[destination][t] += amount;
            }
            return true;
        }
        if (id == HyperCore.USD_CLASS_TRANSFER) {
            (uint64 ntl, bool toPerp) = abi.decode(fields, (uint64, bool));
            if (toPerp) {
                if (spotTotal[sender][0] < uint256(ntl) * 100) return false;
                spotTotal[sender][0] -= ntl * 100;
                rawUsd[sender] += int256(uint256(ntl));
            } else {
                if (withdrawableOf(sender) < ntl) return false;
                rawUsd[sender] -= int256(uint256(ntl));
                spotTotal[sender][0] += ntl * 100;
            }
            return true;
        }
        if (id == HyperCore.ADD_API_WALLET) {
            (address wallet, string memory name) = abi.decode(fields, (address, string));
            agentNamed[sender][keccak256(bytes(_baseName(name)))] = wallet;
            lastAgentName[sender] = name;
            return true;
        }
        if (id == HyperCore.SET_ABSTRACTION) {
            (address user, uint8 mode) = abi.decode(fields, (address, uint8));
            if (user != sender) return false;
            abstraction[sender] = mode;
            return true;
        }
        return false;
    }

    function _order(address sender, bytes memory fields) private returns (bool) {
        (uint32 asset, bool isBuy, uint64 limitPx, uint64 sz, bool reduceOnly,,) =
            abi.decode(fields, (uint32, bool, uint64, uint64, bool, uint8, uint128));
        if (asset >= HyperCore.SPOT_ASSET_OFFSET) {
            SpotPair memory p = pair[asset - HyperCore.SPOT_ASSET_OFFSET];
            Token memory t = token[p.base];
            uint256 lots = sz / 10 ** (8 - t.szDecimals);
            uint256 px = limitPx / 10 ** t.szDecimals;
            if (depth != 0 && lots > depth) lots = depth;
            if (isBuy || p.bid < px || lots == 0) return false;
            uint256 wei_ = lots * 10 ** (t.weiDecimals - t.szDecimals);
            if (spotTotal[sender][p.base] < wei_) return false;
            spotTotal[sender][p.base] -= uint64(wei_);
            spotTotal[sender][0] += uint64(lots * p.bid);
            return true;
        }
        Market memory m = perp[asset];
        int256 lotsOrdered = int256(uint256(sz / 10 ** (8 - m.szDecimals)));
        uint256 price = limitPx / 10 ** (2 + m.szDecimals);
        if (depth != 0 && uint256(lotsOrdered) > depth) lotsOrdered = int256(depth);
        int256 held = szi[sender][asset];
        if (reduceOnly) {
            if (isBuy ? held >= 0 : held <= 0) return false;
            int256 open = held > 0 ? held : -held;
            if (lotsOrdered > open) lotsOrdered = open;
        }
        if (isBuy ? m.ask > price : m.bid < price) return false;
        uint256 fillPx = isBuy ? m.ask : m.bid;
        int256 delta = isBuy ? lotsOrdered : -lotsOrdered;
        szi[sender][asset] = int64(held + delta);
        rawUsd[sender] -= delta * int256(fillPx);
        return true;
    }

    function _baseName(string memory name) private pure returns (string memory) {
        bytes memory b = bytes(name);
        for (uint256 i = 0; i < b.length; i++) {
            if (b[i] == " ") {
                bytes memory head = new bytes(i);
                for (uint256 j = 0; j < i; j++) {
                    head[j] = b[j];
                }
                return string(head);
            }
        }
        return name;
    }
}

/// What sits at each precompile address: it asks the mock.
contract PrecompileStub {
    MockHyperCore private immutable core;

    constructor(MockHyperCore core_) {
        core = core_;
    }

    fallback(bytes calldata input) external returns (bytes memory) {
        return core.answer(address(this), input);
    }
}

/// What sits at CoreWriter's address: it queues the action for the mock.
contract CoreWriterStub {
    MockHyperCore private immutable core;

    constructor(MockHyperCore core_) {
        core = core_;
    }

    function sendRawAction(bytes calldata data) external {
        core.enqueue(msg.sender, data);
    }
}

/// Circle's deposit contract, for tests: dollars in, to the caller's perp
/// balance.
contract MockDepositWallet {
    MockHyperCore private immutable core;
    LocalUsd private immutable usd;

    constructor(MockHyperCore core_, LocalUsd usd_) {
        core = core_;
        usd = usd_;
    }

    function deposit(uint256 amount, uint32) external {
        usd.transferFrom(msg.sender, address(this), amount);
        core.credit(msg.sender, int256(amount));
    }
}
