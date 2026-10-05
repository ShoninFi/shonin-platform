// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// HyperCore as HyperEVM sees it: CoreWriter, which queues an action for the
/// calling contract's own HyperCore account, and the read precompiles, which
/// report HyperCore's state as of the start of the block.
///
/// Two facts shape everything here. CoreWriter never reverts when HyperCore
/// refuses an action, so nothing written is known to have happened until a
/// later block reads it back. And a precompile given a bad input burns all
/// the gas it was given, so each read is made with an explicit, small budget.
library HyperCore {
    address internal constant CORE_WRITER = 0x3333333333333333333333333333333333333333;

    address internal constant POSITION = 0x0000000000000000000000000000000000000800;
    address internal constant SPOT_BALANCE = 0x0000000000000000000000000000000000000801;
    address internal constant VAULT_EQUITY = 0x0000000000000000000000000000000000000802;
    address internal constant WITHDRAWABLE = 0x0000000000000000000000000000000000000803;
    address internal constant MARK_PX = 0x0000000000000000000000000000000000000806;
    address internal constant ORACLE_PX = 0x0000000000000000000000000000000000000807;
    address internal constant SPOT_PX = 0x0000000000000000000000000000000000000808;
    address internal constant PERP_ASSET_INFO = 0x000000000000000000000000000000000000080a;
    address internal constant SPOT_INFO = 0x000000000000000000000000000000000000080b;
    address internal constant TOKEN_INFO = 0x000000000000000000000000000000000000080C;
    address internal constant BBO = 0x000000000000000000000000000000000000080e;
    address internal constant ACCOUNT_MARGIN_SUMMARY = 0x000000000000000000000000000000000000080F;
    address internal constant CORE_USER_EXISTS = 0x0000000000000000000000000000000000000810;

    /// A read costs 2,000 gas plus 65 a byte in and out. These budgets cover
    /// each fixed-size read with room to spare, and the info reads, whose
    /// answers carry a name, for names far longer than any market's.
    uint256 internal constant GAS_SMALL = 12_000;
    uint256 internal constant GAS_POSITION = 25_000;
    uint256 internal constant GAS_SUMMARY = 20_000;
    uint256 internal constant GAS_INFO = 60_000;

    /// Spot order assets are numbered from here, by the pair's index.
    uint32 internal constant SPOT_ASSET_OFFSET = 10_000;
    /// The perp dex every market here is on: the venue's own, never a
    /// builder-deployed one.
    uint32 internal constant MAIN_DEX = 0;

    uint24 internal constant LIMIT_ORDER = 1;
    uint24 internal constant VAULT_TRANSFER = 2;
    uint24 internal constant SPOT_SEND = 6;
    uint24 internal constant USD_CLASS_TRANSFER = 7;
    uint24 internal constant ADD_API_WALLET = 9;
    uint24 internal constant SET_ABSTRACTION = 16;

    uint8 internal constant TIF_IOC = 3;

    error ReadFailed(address precompile);

    struct Position {
        int64 szi;
        uint64 entryNtl;
        int64 isolatedRawUsd;
        uint32 leverage;
        bool isIsolated;
    }

    struct SpotBalance {
        uint64 total;
        uint64 hold;
        uint64 entryNtl;
    }

    struct MarginSummary {
        int64 accountValue;
        uint64 marginUsed;
        uint64 ntlPos;
        int64 rawUsd;
    }

    struct PerpAssetInfo {
        string coin;
        uint32 marginTableId;
        uint8 szDecimals;
        uint8 maxLeverage;
        bool onlyIsolated;
    }

    struct SpotInfo {
        string name;
        uint64[2] tokens;
    }

    struct TokenInfo {
        string name;
        uint64[] spots;
        uint64 deployerTradingFeeShare;
        address deployer;
        address evmContract;
        uint8 szDecimals;
        uint8 weiDecimals;
        int8 evmExtraWeiDecimals;
    }

    // Reads -----------------------------------------------------------------

    function _read(
        address precompile,
        bytes memory input,
        uint256 gas
    ) private view returns (bytes memory out) {
        bool ok;
        (ok, out) = precompile.staticcall{gas: gas}(input);
        if (!ok || out.length == 0) revert ReadFailed(precompile);
    }

    function position(address user, uint32 perp) internal view returns (Position memory) {
        return abi.decode(_read(POSITION, abi.encode(user, uint16(perp)), GAS_POSITION), (Position));
    }

    function spotBalance(address user, uint64 token) internal view returns (SpotBalance memory) {
        return abi.decode(_read(SPOT_BALANCE, abi.encode(user, token), GAS_SMALL), (SpotBalance));
    }

    /// What `user` holds in the venue vault `vault`, in millionths of a
    /// dollar, and until when (in milliseconds) it is locked there.
    function vaultEquity(
        address user,
        address vault
    ) internal view returns (uint64 equity, uint64 lockedUntil) {
        return abi.decode(_read(VAULT_EQUITY, abi.encode(user, vault), GAS_SMALL), (uint64, uint64));
    }

    /// What the perp side could move out now, in millionths of a dollar.
    function withdrawable(address user) internal view returns (uint64) {
        return abi.decode(_read(WITHDRAWABLE, abi.encode(user), GAS_SMALL), (uint64));
    }

    function markPx(uint32 perp) internal view returns (uint64) {
        return abi.decode(_read(MARK_PX, abi.encode(perp), GAS_SMALL), (uint64));
    }

    function oraclePx(uint32 perp) internal view returns (uint64) {
        return abi.decode(_read(ORACLE_PX, abi.encode(perp), GAS_SMALL), (uint64));
    }

    function spotPx(uint32 pair) internal view returns (uint64) {
        return abi.decode(_read(SPOT_PX, abi.encode(pair), GAS_SMALL), (uint64));
    }

    /// The best bid and ask of an order asset: a perp's index, or a spot
    /// pair's index plus `SPOT_ASSET_OFFSET`. Zero where a side is empty.
    function bbo(uint32 asset) internal view returns (uint64 bid, uint64 ask) {
        return abi.decode(_read(BBO, abi.encode(asset), GAS_SMALL), (uint64, uint64));
    }

    function marginSummary(address user) internal view returns (MarginSummary memory) {
        return abi.decode(
            _read(ACCOUNT_MARGIN_SUMMARY, abi.encode(MAIN_DEX, user), GAS_SUMMARY), (MarginSummary)
        );
    }

    function perpAssetInfo(uint32 perp) internal view returns (PerpAssetInfo memory) {
        return abi.decode(_read(PERP_ASSET_INFO, abi.encode(perp), GAS_INFO), (PerpAssetInfo));
    }

    function spotInfo(uint32 pair) internal view returns (SpotInfo memory) {
        return abi.decode(_read(SPOT_INFO, abi.encode(pair), GAS_INFO), (SpotInfo));
    }

    function tokenInfo(uint64 token) internal view returns (TokenInfo memory) {
        return abi.decode(_read(TOKEN_INFO, abi.encode(token), GAS_INFO), (TokenInfo));
    }

    function exists(address user) internal view returns (bool) {
        return abi.decode(_read(CORE_USER_EXISTS, abi.encode(user), GAS_SMALL), (bool));
    }

    // Actions, as CoreWriter's raw bytes ------------------------------------

    /// Version 1, the action's id in three bytes, then its fields.
    function _action(uint24 id, bytes memory fields) private pure returns (bytes memory) {
        return bytes.concat(bytes1(uint8(1)), bytes3(id), fields);
    }

    /// A limit order; prices and sizes are the human figure times 10^8.
    function limitOrder(
        uint32 asset,
        bool isBuy,
        uint64 limitPx,
        uint64 sz,
        bool reduceOnly,
        uint8 tif
    ) internal pure returns (bytes memory) {
        return _action(LIMIT_ORDER, abi.encode(asset, isBuy, limitPx, sz, reduceOnly, tif, uint128(0)));
    }

    /// Into (`isDeposit`) or out of a venue vault, in millionths of a dollar.
    function vaultTransfer(address vault, bool isDeposit, uint64 usd) internal pure returns (bytes memory) {
        return _action(VAULT_TRANSFER, abi.encode(vault, isDeposit, usd));
    }

    /// A spot token, in its HyperCore units, to `destination`: another
    /// account, or the token's system address, which moves it to the same
    /// address on HyperEVM.
    function spotSend(
        address destination,
        uint64 token,
        uint64 amount
    ) internal pure returns (bytes memory) {
        return _action(SPOT_SEND, abi.encode(destination, token, amount));
    }

    /// Dollars between the account's perp and spot balances, in millionths.
    function usdClassTransfer(uint64 ntl, bool toPerp) internal pure returns (bytes memory) {
        return _action(USD_CLASS_TRANSFER, abi.encode(ntl, toPerp));
    }

    /// Approve `wallet` as the agent called `name`; the venue reads a
    /// `valid_until <ms>` suffix on the name as the agent's expiry. A name
    /// already in use is taken over by the new wallet.
    function addApiWallet(address wallet, string memory name) internal pure returns (bytes memory) {
        return _action(ADD_API_WALLET, abi.encode(wallet, name));
    }

    /// Set how the account's balances are kept. Once set by the account, an
    /// agent can no longer change it.
    function setAbstraction(address user, uint8 mode) internal pure returns (bytes memory) {
        return _action(SET_ABSTRACTION, abi.encode(user, mode));
    }
}

/// HyperEVM's queue into HyperCore.
interface ICoreWriter {
    function sendRawAction(bytes calldata data) external;
}

/// Circle's deposit contract on HyperEVM: dollars into the caller's own
/// HyperCore account, to its perp balance (`destinationDex` 0) or its spot
/// balance (`type(uint32).max`).
interface ICoreDepositWallet {
    function deposit(uint256 amount, uint32 destinationDex) external;
}
