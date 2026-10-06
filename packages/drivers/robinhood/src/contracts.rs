//! The on-chain calls the executor half makes or reads: the driver's views,
//! the station's spec, Roles v2's role call, Uniswap's swap, Chainlink's
//! feeds and the stock tokens.

alloy::sol! {
    #![sol(all_derives)]
    #[sol(rpc)]
    interface IRobinhoodDriver {
        struct Stock {
            address token;
            address feed;
            address pool;
            uint24 fee;
            uint256 scale;
        }
        function stocks() external view returns (Stock[] memory);
        function marketQuote(address pool, address token, uint256 amount) external view returns (uint256);
        function dollar() external view returns (address);
        function router() external view returns (address);
        function maxFeedAge() external view returns (uint256);
        function safeOf(address station) external view returns (address);
        function rolesOf(address station) external view returns (address);
        function isSetUp(address station) external view returns (bool);
        function valueOf(address station) external view returns (uint256 mark, uint256 exit);
    }

    #[sol(rpc)]
    interface IStationSpec {
        struct StationSpec {
            uint256 vaultChain;
            address vault;
            bytes32 bundleHash;
            bytes32[] drivers;
            bytes32[] bridges;
            uint256[] chains;
            uint16 exitBandBps;
        }
        function spec() external view returns (StationSpec memory);
    }

    #[sol(rpc)]
    interface IRoles {
        function execTransactionWithRole(
            address to,
            uint256 value,
            bytes calldata data,
            uint8 operation,
            bytes32 roleKey,
            bool shouldRevert
        ) external returns (bool success);
    }

    interface ISwapRouter02 {
        struct ExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            uint24 fee;
            address recipient;
            uint256 amountIn;
            uint256 amountOutMinimum;
            uint160 sqrtPriceLimitX96;
        }
        function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
    }

    #[sol(rpc)]
    interface IAggregatorV3 {
        function decimals() external view returns (uint8);
        function description() external view returns (string memory);
        function latestRoundData()
            external
            view
            returns (uint80 roundId, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound);
    }

    #[sol(rpc)]
    interface IStockToken {
        function symbol() external view returns (string memory);
        function decimals() external view returns (uint8);
        function balanceOf(address owner) external view returns (uint256);
        function approve(address spender, uint256 amount) external returns (bool);
        function oraclePaused() external view returns (bool);
        function uiMultiplier() external view returns (uint256);
        event Transfer(address indexed from, address indexed to, uint256 value);
    }
}

/// The role the executor's key is given in the station's Roles module.
pub const ROLE: [u8; 32] = {
    let mut role = [0u8; 32];
    let name = b"shonin-executor";
    let mut i = 0;
    while i < name.len() {
        role[i] = name[i];
        i += 1;
    }
    role
};
