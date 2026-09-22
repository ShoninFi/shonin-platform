// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {
    Fill, Funding, ILocalVenue, Market, OrderRequest, Position, Quote
} from "./interfaces/ILocalVenue.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";

/// The venue a vault's capital sits on.
///
/// It stands where a real exchange stands, and it keeps the two properties the
/// platform actually rests on. An account belongs to one **master**, and only
/// the master can move dollars into or out of it. The master may approve
/// **agents**, which can trade and do nothing else, so a strategy's key can
/// lose money and can never take it.
///
/// What it does not do is keep a book. Every order is immediate-or-cancel
/// against the current bid and ask, because the flows the platform depends on
/// -- entering, reversing, and closing a leaver's fraction inside an exit band
/// -- all cross the spread. A resting order would be a second, larger thing to
/// get right and nothing here would read it.
///
/// Sizes carry 18 decimals and every dollar amount carries the asset's 6, so
/// `size * price / 1e18` is a dollar amount. Cash is signed: buying a perp on
/// margin leaves it negative, which is what leverage is.
contract LocalVenue is ILocalVenue {
    using SafeERC20 for IERC20;

    uint256 private constant SIZE_SCALE = 1e18;
    uint256 private constant BPS = 10_000;

    IERC20 public immutable asset;
    /// Moves prices and accrues funding. A real venue is its own market; here
    /// the development chain plays one.
    address public immutable operator;
    /// What a perp position has to be backed by, in basis points of its
    /// notional. It is the venue's own limit, not the platform's: how much
    /// leverage to use inside it is a strategy's own decision, written in its
    /// own code.
    uint256 public immutable marginBps;

    bytes32[] private _markets;
    mapping(bytes32 => Market) private _market;

    mapping(address => int256) private _cash;
    mapping(address => mapping(bytes32 => Position)) private _position;
    mapping(address => bytes32[]) private _held;
    mapping(address => mapping(bytes32 => bool)) private _isHeld;
    mapping(address => mapping(address => uint64)) private _agentValidUntil;

    error NotOperator();
    error NotMaster();
    error NotPermitted();
    error NoSuchMarket();
    error MarketExists();
    error WouldGoNegative();

    constructor(IERC20 asset_, address operator_, uint256 marginBps_) {
        asset = asset_;
        operator = operator_;
        marginBps = marginBps_;
    }

    modifier onlyOperator() {
        if (msg.sender != operator) revert NotOperator();
        _;
    }

    // Markets and prices ----------------------------------------------------

    function listMarket(bytes32 id, bool isPerp, uint256 price) external onlyOperator {
        if (_market[id].id != bytes32(0)) revert MarketExists();
        _market[id] = Market({
            id: id,
            isPerp: isPerp,
            mark: price,
            oracle: price,
            bid: price,
            ask: price,
            fundingIndex: 0,
            fundingRate: 0,
            fundingUpdatedAt: uint64(block.timestamp)
        });
        _markets.push(id);
        emit PricesSet(id, price, price, price, price);
    }

    function setPrices(
        bytes32 market,
        uint256 mark,
        uint256 oracle,
        uint256 bid,
        uint256 ask
    ) external onlyOperator {
        _setPrices(market, mark, oracle, bid, ask);
    }

    /// Everything a market operator has to say, in one call. A chain that
    /// mines on a timer cannot afford a transaction per market per block.
    function post(Quote[] calldata quotes, Funding[] calldata funding) external onlyOperator {
        for (uint256 i = 0; i < quotes.length; i++) {
            _setPrices(quotes[i].market, quotes[i].mark, quotes[i].oracle, quotes[i].bid, quotes[i].ask);
        }
        for (uint256 i = 0; i < funding.length; i++) {
            _setFunding(funding[i].market, funding[i].ratePerDay);
            _accrueFunding(funding[i].market, funding[i].paidPerUnit);
        }
    }

    function _setPrices(bytes32 market, uint256 mark, uint256 oracle, uint256 bid, uint256 ask) private {
        Market storage m = _market[market];
        if (m.id == bytes32(0)) revert NoSuchMarket();
        m.mark = mark;
        m.oracle = oracle;
        m.bid = bid;
        m.ask = ask;
        emit PricesSet(market, mark, oracle, bid, ask);
    }

    function accrueFunding(bytes32 market, int256 paidPerUnit) external onlyOperator {
        _accrueFunding(market, paidPerUnit);
    }

    function setFunding(bytes32 market, int256 ratePerDay) external onlyOperator {
        _setFunding(market, ratePerDay);
    }

    function _accrueFunding(bytes32 market, int256 paidPerUnit) private {
        Market storage m = _market[market];
        if (m.id == bytes32(0)) revert NoSuchMarket();
        m.fundingIndex += paidPerUnit;
        m.fundingUpdatedAt = uint64(block.timestamp);
        emit FundingAccrued(market, paidPerUnit, m.fundingIndex);
    }

    function _setFunding(bytes32 market, int256 ratePerDay) private {
        Market storage m = _market[market];
        if (m.id == bytes32(0)) revert NoSuchMarket();
        m.fundingRate = ratePerDay;
        emit FundingSet(market, ratePerDay);
    }

    function markets() external view returns (bytes32[] memory) {
        return _markets;
    }

    function marketOf(bytes32 market) external view returns (Market memory) {
        return _market[market];
    }

    // Agents ----------------------------------------------------------------

    function approveAgent(address agent, uint64 validUntil) external {
        _agentValidUntil[msg.sender][agent] = validUntil;
        emit AgentApproved(msg.sender, agent, validUntil);
    }

    function revokeAgent(address agent) external {
        _agentValidUntil[msg.sender][agent] = 0;
        emit AgentRevoked(msg.sender, agent);
    }

    function isAgent(address master, address agent) public view returns (bool) {
        return _agentValidUntil[master][agent] >= block.timestamp;
    }

    function agentValidUntil(address master, address agent) external view returns (uint64) {
        return _agentValidUntil[master][agent];
    }

    // Dollars in and out ----------------------------------------------------

    /// Master only. There is no call that sends the account's dollars anywhere
    /// but back to its master, which is the whole of the custody claim.
    function deposit(uint256 amount) external {
        asset.safeTransferFrom(msg.sender, address(this), amount);
        _cash[msg.sender] += int256(amount);
        emit Deposited(msg.sender, amount);
    }

    function withdraw(uint256 amount) external {
        _settleAllFunding(msg.sender);
        _cash[msg.sender] -= int256(amount);
        int256 left = _valueAt(msg.sender, PriceKind.Mark);
        if (_cash[msg.sender] < 0 || left < 0) revert WouldGoNegative();
        if (uint256(left) * BPS < grossPerpNotional(msg.sender) * marginBps) revert WouldGoNegative();
        asset.safeTransfer(msg.sender, amount);
        emit Withdrawn(msg.sender, amount);
    }

    // Trading ---------------------------------------------------------------

    function place(address master, OrderRequest[] calldata orders) external returns (Fill[] memory) {
        if (msg.sender != master && !isAgent(master, msg.sender)) revert NotPermitted();
        Fill[] memory fills = new Fill[](orders.length);
        for (uint256 i = 0; i < orders.length; i++) {
            fills[i] = _place(master, orders[i]);
        }
        return fills;
    }

    function _place(address master, OrderRequest calldata order) private returns (Fill memory) {
        Market storage m = _market[order.market];
        if (m.id == bytes32(0)) return _reject(master, order.market, "no such market");
        if (order.size == 0) return _reject(master, order.market, "zero size");

        _settleFunding(master, order.market);
        Position storage p = _position[master][order.market];

        uint256 size = order.size;
        if (order.reduceOnly) {
            if (p.size == 0) return _reject(master, order.market, "nothing to reduce");
            bool shrinks = order.isBuy ? p.size < 0 : p.size > 0;
            if (!shrinks) return _reject(master, order.market, "would not reduce");
            uint256 open = uint256(p.size >= 0 ? p.size : -p.size);
            if (size > open) size = open;
        }

        uint256 price = order.isBuy ? m.ask : m.bid;
        if (order.isBuy ? price > order.limitPrice : price < order.limitPrice) {
            return _reject(master, order.market, "price outside the limit");
        }

        // What the account would look like afterwards. Nothing is written
        // until every check has read it, so a rejected order leaves no trace
        // and the orders beside it in the batch keep their fills.
        uint256 notional = (size * price) / SIZE_SCALE;
        int256 signedSize = int256(size);
        int256 signedNotional = int256(notional);
        int256 nextCash = order.isBuy ? _cash[master] - signedNotional : _cash[master] + signedNotional;
        int256 nextSize = order.isBuy ? p.size + signedSize : p.size - signedSize;
        int256 atMark = (signedSize * int256(m.mark)) / int256(SIZE_SCALE);
        int256 nextValue = _valueAt(master, PriceKind.Mark)
            + (order.isBuy ? atMark - signedNotional : signedNotional - atMark);

        if (!m.isPerp && (nextSize < 0 || nextCash < 0)) {
            return _reject(master, order.market, "spot cannot be borrowed against");
        }
        if (nextValue < 0) {
            return _reject(master, order.market, "would leave the account underwater");
        }
        if (
            m.isPerp
                && uint256(nextValue) * BPS < _grossPerpNotionalWith(master, order.market, nextSize) * marginBps
        ) {
            return _reject(master, order.market, "not enough margin");
        }

        _cash[master] = nextCash;
        p.size = nextSize;
        _remember(master, order.market);

        emit Filled(master, order.market, msg.sender, order.isBuy, order.reduceOnly, size, price, notional);
        return Fill({accepted: true, reason: "", size: size, price: price, notional: notional});
    }

    /// An order the venue would not take. The batch it came in carries on, so
    /// a strategy submitting several orders learns which of them landed.
    function _reject(address master, bytes32 market, string memory reason) private returns (Fill memory) {
        emit Rejected(master, market, msg.sender, reason);
        return Fill({accepted: false, reason: reason, size: 0, price: 0, notional: 0});
    }

    // Value -----------------------------------------------------------------

    enum PriceKind {
        Mark,
        Exit
    }

    function accountValue(address master) external view returns (uint256) {
        int256 value = _valueAt(master, PriceKind.Mark);
        return value > 0 ? uint256(value) : 0;
    }

    function exitValue(address master) external view returns (uint256) {
        int256 value = _valueAt(master, PriceKind.Exit);
        return value > 0 ? uint256(value) : 0;
    }

    function cashOf(address master) external view returns (int256) {
        return _cash[master];
    }

    function positionOf(address master, bytes32 market) external view returns (Position memory) {
        return _position[master][market];
    }

    function marketsHeldBy(address master) external view returns (bytes32[] memory) {
        return _held[master];
    }

    function grossPerpNotional(address master) public view returns (uint256) {
        return _grossPerpNotionalWith(master, bytes32(0), 0);
    }

    /// The same sum, with one market's size replaced by what a pending order
    /// would leave there.
    function _grossPerpNotionalWith(
        address master,
        bytes32 market,
        int256 size
    ) private view returns (uint256) {
        uint256 gross;
        bytes32[] storage held = _held[master];
        for (uint256 i = 0; i < held.length; i++) {
            Market storage m = _market[held[i]];
            if (!m.isPerp) continue;
            int256 open = held[i] == market ? size : _position[master][held[i]].size;
            if (open == 0) continue;
            uint256 magnitude = uint256(open > 0 ? open : -open);
            gross += (magnitude * m.mark) / SIZE_SCALE;
        }
        if (market != bytes32(0) && !_isHeld[master][market] && size != 0 && _market[market].isPerp) {
            uint256 magnitude = uint256(size > 0 ? size : -size);
            gross += (magnitude * _market[market].mark) / SIZE_SCALE;
        }
        return gross;
    }

    /// Cash, plus what every position is worth, less the funding it owes but
    /// has not yet settled.
    function _valueAt(address master, PriceKind kind) private view returns (int256) {
        int256 value = _cash[master];
        bytes32[] storage held = _held[master];
        for (uint256 i = 0; i < held.length; i++) {
            Market storage m = _market[held[i]];
            Position storage p = _position[master][held[i]];
            if (p.size == 0) continue;
            uint256 price = m.mark;
            if (kind == PriceKind.Exit) price = p.size > 0 ? m.bid : m.ask;
            value += (p.size * int256(price)) / int256(SIZE_SCALE);
            value -= (p.size * (m.fundingIndex - p.fundingIndex)) / int256(SIZE_SCALE);
        }
        return value;
    }

    // Funding ---------------------------------------------------------------

    function _settleFunding(address master, bytes32 market) private {
        Market storage m = _market[market];
        Position storage p = _position[master][market];
        if (p.fundingIndex == m.fundingIndex) return;
        if (p.size != 0) {
            _cash[master] -= (p.size * (m.fundingIndex - p.fundingIndex)) / int256(SIZE_SCALE);
        }
        p.fundingIndex = m.fundingIndex;
    }

    function _settleAllFunding(address master) private {
        bytes32[] storage held = _held[master];
        for (uint256 i = 0; i < held.length; i++) {
            _settleFunding(master, held[i]);
        }
    }

    function _remember(address master, bytes32 market) private {
        if (_isHeld[master][market]) return;
        _isHeld[master][market] = true;
        _held[master].push(market);
    }
}
