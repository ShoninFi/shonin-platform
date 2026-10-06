// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

/// Lighter's exchange contract as a station sees it, with the exchange's
/// off-chain engine reduced to `process`: requests are queued on-chain as
/// the real one queues them, and carried out only when the test says the
/// exchange has got to them. One perp market's worth of state per account.
contract MockZkLighter {
    uint16 public constant DOLLAR_ASSET = 3;

    struct Order {
        uint48 account;
        uint16 market;
        uint48 baseAmount;
        uint32 price;
        bool isAsk;
    }

    struct Withdrawal {
        uint48 account;
        uint64 amount;
    }

    IERC20 public immutable dollar;
    uint48 public lastAccountIndex = 100;
    mapping(address => uint48) public addressToAccountIndex;
    mapping(uint48 => address) public ownerOf;
    mapping(uint48 => mapping(uint8 => bytes)) public pubKey;
    /// Cash per account, in dollar units, and positions per account and
    /// market, with the price they were opened at.
    mapping(uint48 => int256) public cash;
    mapping(uint48 => mapping(uint16 => int64)) public size;
    mapping(uint48 => mapping(uint16 => uint32)) public entry;
    mapping(uint16 => uint32) public markOf;
    /// 10^(size decimals + price decimals) per market.
    mapping(uint16 => uint256) public unit;
    mapping(address => uint128) private _pending;
    /// Dollars carried out of an account and not yet credited to its
    /// owner's pending balance, while credits wait for `credit`.
    mapping(address => uint128) public inFlightOf;
    address[] private _owed;

    Order[] public orders;
    Withdrawal[] public withdrawals;
    uint256 public cancels;
    uint256 public priorityRequests;
    /// Whether a withdrawal larger than the account can give is refused
    /// whole, rather than trimmed to what it can.
    bool public refuseOversized;
    /// Whether a withdrawal carried out reaches the pending balance only at
    /// `credit` (the batch executed on-chain, minutes later, as the real
    /// exchange does), rather than at once.
    bool public creditLater;

    error NotRegistered();
    error NotOwner();

    constructor(IERC20 dollar_) {
        dollar = dollar_;
    }

    function listMarket(uint16 market, uint8 sizeDecimals, uint8 priceDecimals, uint32 mark) external {
        unit[market] = 10 ** (uint256(sizeDecimals) + priceDecimals);
        markOf[market] = mark;
    }

    function setRefuseOversized(bool refuse) external {
        refuseOversized = refuse;
    }

    function setCreditLater(bool later) external {
        creditLater = later;
    }

    function setMark(uint16 market, uint32 mark) external {
        markOf[market] = mark;
    }

    // What the station calls ------------------------------------------------

    function deposit(address to, uint16 assetIndex, uint8, uint256 amount) external payable {
        require(assetIndex == DOLLAR_ASSET, "asset");
        dollar.transferFrom(msg.sender, address(this), amount);
        uint48 account = addressToAccountIndex[to];
        if (account == 0) {
            account = ++lastAccountIndex;
            addressToAccountIndex[to] = account;
            ownerOf[account] = to;
        }
        cash[account] += int256(amount);
        priorityRequests++;
    }

    function changePubKey(uint48 accountIndex, uint8 apiKeyIndex, bytes calldata key) external {
        uint48 master = _master(msg.sender);
        if (accountIndex == type(uint48).max) accountIndex = master;
        if (ownerOf[accountIndex] != msg.sender) revert NotOwner();
        pubKey[accountIndex][apiKeyIndex] = key;
        priorityRequests++;
    }

    function createOrder(
        uint48 accountIndex,
        uint16 marketIndex,
        uint48 baseAmount,
        uint32 price,
        uint8 isAsk,
        uint8
    ) external {
        _master(msg.sender);
        orders.push(Order(accountIndex, marketIndex, baseAmount, price, isAsk == 1));
        priorityRequests++;
    }

    function cancelAllOrders(uint48) external {
        _master(msg.sender);
        cancels++;
        priorityRequests++;
    }

    function withdraw(uint48 accountIndex, uint16, uint8, uint64 baseAmount) external {
        _master(msg.sender);
        withdrawals.push(Withdrawal(accountIndex, baseAmount));
        priorityRequests++;
    }

    function withdrawPendingBalance(address owner, uint16, uint128 baseAmount) external {
        require(baseAmount != 0 && baseAmount <= _pending[owner], "amount");
        _pending[owner] -= baseAmount;
        dollar.transfer(owner, baseAmount);
    }

    function burnShares(uint48, uint48, uint64) external {
        _master(msg.sender);
        priorityRequests++;
    }

    function getPendingBalance(address owner, uint16) external view returns (uint128) {
        return _pending[owner];
    }

    function desertMode() external pure returns (bool) {
        return false;
    }

    // The exchange's engine -------------------------------------------------

    /// What the executor's key does on the account: trade at the mark.
    function trade(uint48 account, uint16 market, int64 delta) external {
        require(size[account][market] == 0, "one open at a time");
        size[account][market] = delta;
        entry[account][market] = markOf[market];
    }

    /// Carry out the queued requests: reduce-only IOC orders fill at the mark
    /// when the limit allows; withdrawals become pending balances.
    function process() external {
        for (uint256 i = 0; i < orders.length; i++) {
            Order memory o = orders[i];
            int64 held = size[o.account][o.market];
            uint32 mark = markOf[o.market];
            bool closes = o.isAsk ? held > 0 : held < 0;
            if (!closes) continue;
            if (o.isAsk ? mark < o.price : mark > o.price) continue;
            uint64 open = uint64(held > 0 ? held : -held);
            uint64 amount = o.baseAmount == 0 || o.baseAmount > open ? open : uint64(o.baseAmount);
            int256 pnl = (int256(uint256(mark)) - int256(uint256(entry[o.account][o.market])))
                * int256(uint256(amount)) * 1e6 / int256(unit[o.market]);
            if (held < 0) pnl = -pnl;
            cash[o.account] += pnl;
            size[o.account][o.market] = held > 0 ? held - int64(amount) : held + int64(amount);
        }
        delete orders;
        for (uint256 i = 0; i < withdrawals.length; i++) {
            Withdrawal memory w = withdrawals[i];
            int256 free = cash[w.account] - _margin(w.account);
            uint256 amount = free <= 0 ? 0 : (uint256(free) < w.amount ? uint256(free) : w.amount);
            if (refuseOversized && amount < w.amount) amount = 0;
            cash[w.account] -= int256(amount);
            address owner = ownerOf[w.account];
            if (creditLater) {
                if (inFlightOf[owner] == 0 && amount != 0) _owed.push(owner);
                inFlightOf[owner] += uint128(amount);
            } else {
                _pending[owner] += uint128(amount);
            }
        }
        delete withdrawals;
    }

    /// The batch executed on-chain: what was carried out reaches the
    /// owners' pending balances.
    function credit() external {
        for (uint256 i = 0; i < _owed.length; i++) {
            address owner = _owed[i];
            _pending[owner] += inFlightOf[owner];
            inFlightOf[owner] = 0;
        }
        delete _owed;
    }

    /// The account's worth at the mark, in dollar units.
    function equity(uint48 account, uint16[] calldata markets) public view returns (int256 value) {
        value = cash[account];
        for (uint256 i = 0; i < markets.length; i++) {
            int64 held = size[account][markets[i]];
            if (held == 0) continue;
            value += (int256(uint256(markOf[markets[i]])) - int256(uint256(entry[account][markets[i]])))
                * int256(held) * 1e6 / int256(unit[markets[i]]);
        }
    }

    function orderCount() external view returns (uint256) {
        return orders.length;
    }

    function withdrawalCount() external view returns (uint256) {
        return withdrawals.length;
    }

    function _margin(uint48) private pure returns (int256) {
        return 0;
    }

    function _master(address who) private view returns (uint48 account) {
        account = addressToAccountIndex[who];
        if (account == 0) revert NotRegistered();
    }
}
