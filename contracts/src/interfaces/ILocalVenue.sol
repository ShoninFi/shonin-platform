// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// A market on the venue. Sizes carry 18 decimals, prices and every dollar
/// amount carry the asset's 6, so `size * price / 1e18` is a dollar amount.
struct Market {
    bytes32 id;
    bool isPerp;
    /// What the venue values a position at.
    uint256 mark;
    /// What the venue's own price source says, which bounds an exit.
    uint256 oracle;
    uint256 bid;
    uint256 ask;
    /// Dollars per unit of size that longs have paid shorts since the market
    /// opened. A position's own share of it is settled when it next moves.
    int256 fundingIndex;
    /// What longs are paying shorts now, in dollars per unit of size per day.
    /// Positive means the carry favours a short.
    int256 fundingRate;
    uint64 fundingUpdatedAt;
}

/// What one account holds in one market.
struct Position {
    /// Positive is long, negative is short.
    int256 size;
    /// The funding index this position last settled at.
    int256 fundingIndex;
}

/// One order, as a strategy submits it. Every order is immediate-or-cancel
/// against the venue's current bid and ask: this venue keeps no book, because
/// nothing the platform depends on needs one.
struct OrderRequest {
    bytes32 market;
    bool isBuy;
    uint256 size;
    /// The worst price the order may fill at.
    uint256 limitPrice;
    /// Only ever shrinks a position, and never past flat.
    bool reduceOnly;
}

/// What the venue did with one order.
struct Fill {
    bool accepted;
    /// Why it was not accepted, when it was not.
    string reason;
    uint256 size;
    uint256 price;
    uint256 notional;
}

/// One market's prices, as the operator posts them.
struct Quote {
    bytes32 market;
    uint256 mark;
    uint256 oracle;
    uint256 bid;
    uint256 ask;
}

/// What longs are paying shorts in one market, and what has accrued since the
/// last posting.
struct Funding {
    bytes32 market;
    int256 ratePerDay;
    int256 paidPerUnit;
}

/// The venue a vault's capital sits on: it holds the account, it decides what
/// the account is worth, and it is what stops a trading key withdrawing.
///
/// An account belongs to one master address. The master may approve **agents**
/// that can trade the account and do nothing else: an agent cannot deposit,
/// withdraw, or approve another agent. That is the whole custody argument, and
/// it is enforced here rather than promised anywhere.
interface ILocalVenue {
    event AgentApproved(address indexed master, address indexed agent, uint64 validUntil);
    event AgentRevoked(address indexed master, address indexed agent);
    event Deposited(address indexed master, uint256 amount);
    event Withdrawn(address indexed master, uint256 amount);
    event Filled(
        address indexed master,
        bytes32 indexed market,
        address indexed by,
        bool isBuy,
        bool reduceOnly,
        uint256 size,
        uint256 price,
        uint256 notional
    );
    event Rejected(address indexed master, bytes32 indexed market, address indexed by, string reason);
    event PricesSet(bytes32 indexed market, uint256 mark, uint256 oracle, uint256 bid, uint256 ask);
    event FundingAccrued(bytes32 indexed market, int256 paidPerUnit, int256 fundingIndex);
    event FundingSet(bytes32 indexed market, int256 ratePerDay);

    /// Approve a key to trade this account until `validUntil`. Master only.
    function approveAgent(address agent, uint64 validUntil) external;
    /// Master only.
    function revokeAgent(address agent) external;
    /// Whether this key may trade this account right now.
    function isAgent(address master, address agent) external view returns (bool);
    function agentValidUntil(address master, address agent) external view returns (uint64);

    /// Move the asset into the account. Master only: the account's dollars can
    /// only ever come from, and return to, the master.
    function deposit(uint256 amount) external;
    /// Master only.
    function withdraw(uint256 amount) external;

    /// Submit orders for `master`'s account. Callable by the master itself or
    /// by one of its agents, and by nobody else.
    function place(address master, OrderRequest[] calldata orders) external returns (Fill[] memory);

    /// What the account is worth at mark, in the asset.
    function accountValue(address master) external view returns (uint256);
    /// What it is worth to someone leaving: longs at the bid, shorts at the ask.
    function exitValue(address master) external view returns (uint256);
    /// Signed: buying a perp on margin leaves it negative, which is what
    /// leverage is.
    function cashOf(address master) external view returns (int256);
    function positionOf(address master, bytes32 market) external view returns (Position memory);
    /// Every market a master holds a position in.
    function marketsHeldBy(address master) external view returns (bytes32[] memory);

    /// The equity a perp position must be backed by, in basis points of its
    /// notional. Spot is paid for in full and needs none.
    function marginBps() external view returns (uint256);
    /// What every open perp position is worth at mark, added up without
    /// regard to sign: what the account has to hold margin against.
    function grossPerpNotional(address master) external view returns (uint256);

    function markets() external view returns (bytes32[] memory);
    function marketOf(bytes32 market) external view returns (Market memory);

    /// The market operator moves prices and accrues funding. On a real venue
    /// this is the market itself; here it is the development chain.
    function setPrices(bytes32 market, uint256 mark, uint256 oracle, uint256 bid, uint256 ask) external;
    /// Longs pay shorts `paidPerUnit` dollars for each unit of size held.
    function accrueFunding(bytes32 market, int256 paidPerUnit) external;
    /// Publish what longs are paying shorts, per unit of size per day.
    function setFunding(bytes32 market, int256 ratePerDay) external;
    function listMarket(bytes32 id, bool isPerp, uint256 price) external;
    /// Everything a market operator has to say in one call, so a chain that
    /// mines on a timer keeps up with itself.
    function post(Quote[] calldata quotes, Funding[] calldata funding) external;
}
