//! `testnet-market`: keeps the test networks' pools at the real market
//! price, so every agent on a test network trades a real exchange's software
//! at real prices.
//!
//! Every round (a minute by default) it reads each Robinhood stock's
//! Chainlink feed on Robinhood Chain mainnet, with no key, and when the feed
//! has a new round posts it to the stock's mirror feed on the testnet; then
//! for every pool it keeps (each stock against USDG on Robinhood Chain
//! testnet, WETH against USDC on Base Sepolia) it works out the swap that
//! brings the pool to the price the vaults value at, from the pool's price
//! and the liquidity in the ticks the swap crosses ([`math`]), caps it, and
//! sends it through Uniswap's SwapRouter02 with the target as its price
//! limit, so a swap never carries a pool past the price.
//!
//! It holds one key, a test network account of its own, and refuses
//! mainnet and the local chains. Its inventory is the account's: USDG is
//! minted as it is needed (anyone may mint the testnet's), while stock
//! tokens, WETH and USDC are finite, so it never sells one below its floor,
//! says at warn when it can't move a pool one way, and buys back what it
//! sold when the price lets it ([`market`]).

pub mod health;
pub mod market;
pub mod math;

/// The account's index along `m/44'/60'/0'/0/<i>` of the test networks' EVM
/// mnemonic: fixed, after the funder's, and never one of the platform's
/// roles.
pub const MARKET_INDEX: u32 = 101;

/// The variable the account's key (a `0x` private key) is handed in.
pub const KEY_ENV: &str = "SHONIN_TESTNET_MARKET_KEY";

/// The test networks' EVM mnemonic, which the key is derived from when it
/// isn't handed over (the same variable the platform's tools read).
pub const MNEMONIC_ENV: &str = "SHONIN_TESTNET_EVM_MNEMONIC";

/// The contracts it reads and calls.
pub mod bindings {
    // Chainlink's feed interface (`AggregatorV3Interface`, as read on
    // mainnet, on Base Sepolia and on a mirror) and the mirror's own.
    alloy::sol!(
        #[sol(rpc)]
        "../../contracts/evm/test/testnet/IMirrorFeed.sol"
    );

    alloy::sol! {
        /// A Uniswap v3 pool, as read.
        #[sol(rpc)]
        interface IUniswapV3Pool {
            function token0() external view returns (address);
            function token1() external view returns (address);
            function fee() external view returns (uint24);
            function tickSpacing() external view returns (int24);
            function liquidity() external view returns (uint128);
            function slot0()
                external
                view
                returns (
                    uint160 sqrtPriceX96,
                    int24 tick,
                    uint16 observationIndex,
                    uint16 observationCardinality,
                    uint16 observationCardinalityNext,
                    uint8 feeProtocol,
                    bool unlocked
                );
            function tickBitmap(int16 wordPosition) external view returns (uint256);
            function ticks(int24 tick)
                external
                view
                returns (
                    uint128 liquidityGross,
                    int128 liquidityNet,
                    uint256 feeGrowthOutside0X128,
                    uint256 feeGrowthOutside1X128,
                    int56 tickCumulativeOutside,
                    uint160 secondsPerLiquidityOutsideX128,
                    uint32 secondsOutside,
                    bool initialized
                );
        }

        /// SwapRouter02's one swap.
        #[sol(rpc)]
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

        /// A token, and the testnet dollar's open mint.
        #[sol(rpc)]
        interface IToken {
            function decimals() external view returns (uint8);
            function balanceOf(address owner) external view returns (uint256);
            function allowance(address owner, address spender) external view returns (uint256);
            function approve(address spender, uint256 amount) external returns (bool);
            function transfer(address to, uint256 amount) external returns (bool);
            function mint(address to, uint256 amount) external;
        }
    }
}
