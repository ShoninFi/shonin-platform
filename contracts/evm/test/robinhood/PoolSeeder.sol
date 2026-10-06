// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

interface IUniswapV3FactoryLite {
    function createPool(address tokenA, address tokenB, uint24 fee) external returns (address pool);
}

interface IUniswapV3PoolLite {
    function initialize(uint160 sqrtPriceX96) external;
    function token0() external view returns (address);
    function mint(
        address recipient,
        int24 tickLower,
        int24 tickUpper,
        uint128 amount,
        bytes calldata data
    ) external returns (uint256 amount0, uint256 amount1);
}

/// Makes a Uniswap v3 pool of a stock token against a dollar on a fork, at a
/// given price, with liquidity across the whole range: for tests whose dollar
/// has no real pool.
contract PoolSeeder {
    int24 private constant MIN_TICK = -887_270;
    int24 private constant MAX_TICK = 887_270;

    /// `price` in dollars per whole token with `priceDecimals`; the token has
    /// 18 decimals and the dollar `dollarDecimals`.
    function seed(
        IUniswapV3FactoryLite factory,
        address token,
        address dollar,
        uint24 fee,
        uint256 price,
        uint8 priceDecimals,
        uint8 dollarDecimals,
        uint128 liquidity
    ) external returns (address pool) {
        pool = factory.createPool(token, dollar, fee);
        // The pool's price is token1 per token0, in raw units.
        uint256 num = price * 10 ** dollarDecimals;
        uint256 den = 10 ** (uint256(priceDecimals) + 18);
        uint256 ratioX192 = token < dollar ? Math.mulDiv(num, 1 << 192, den) : Math.mulDiv(den, 1 << 192, num);
        IUniswapV3PoolLite(pool).initialize(uint160(Math.sqrt(ratioX192)));
        IUniswapV3PoolLite(pool).mint(msg.sender, MIN_TICK, MAX_TICK, liquidity, abi.encode(token, dollar));
    }

    function uniswapV3MintCallback(uint256 amount0, uint256 amount1, bytes calldata data) external {
        (address token, address dollar) = abi.decode(data, (address, address));
        (address token0, address token1) = token < dollar ? (token, dollar) : (dollar, token);
        if (amount0 != 0) IERC20(token0).transfer(msg.sender, amount0);
        if (amount1 != 0) IERC20(token1).transfer(msg.sender, amount1);
    }
}
