// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// The calls of Lighter's exchange contract on Robinhood Chain that the
/// Lighter driver makes or reads. Every one that acts is a priority request:
/// it is queued on-chain in the transaction and carried out by the exchange
/// in seconds (orders, keys) or minutes (withdrawals), and a request left
/// unexecuted for 14 days lets anyone freeze the exchange and exit by proof.
interface IZkLighter {
    /// Credit `amount` of asset `assetIndex` to `to`'s account, creating the
    /// account if `to` has none: the first deposit makes an address the
    /// owner of a new master account.
    function deposit(address to, uint16 assetIndex, uint8 routeType, uint256 amount) external payable;

    /// Set the public key in slot `apiKeyIndex` of account `accountIndex`
    /// (one the caller owns), replacing whatever key was there. The
    /// all-ones index means the caller's own master account.
    function changePubKey(uint48 accountIndex, uint8 apiKeyIndex, bytes calldata pubKey) external;

    /// A reduce-only, immediate-or-cancel order on one of the caller's
    /// accounts. A zero amount means the whole position.
    function createOrder(
        uint48 accountIndex,
        uint16 marketIndex,
        uint48 baseAmount,
        uint32 price,
        uint8 isAsk,
        uint8 orderType
    ) external;

    function cancelAllOrders(uint48 accountIndex) external;

    /// Withdraw from one of the caller's accounts to the caller: it becomes
    /// a pending balance once carried out, which anyone may then pay out.
    function withdraw(uint48 accountIndex, uint16 assetIndex, uint8 routeType, uint64 baseAmount) external;

    /// Pay `owner`'s pending balance of an asset to `owner`. Anyone may.
    function withdrawPendingBalance(address owner, uint16 assetIndex, uint128 baseAmount) external;

    /// Burn the caller's account's shares in a public pool.
    function burnShares(uint48 accountIndex, uint48 publicPoolIndex, uint64 shareAmount) external;

    function getPendingBalance(address owner, uint16 assetIndex) external view returns (uint128);
    function addressToAccountIndex(address owner) external view returns (uint48);
    function desertMode() external view returns (bool);
}
