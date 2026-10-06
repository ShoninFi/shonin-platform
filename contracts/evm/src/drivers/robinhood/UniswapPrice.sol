// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IUniswapV3Pool} from "./RobinhoodChain.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// A Uniswap v3 pool's time-weighted price: what the pool says over a window
/// of minutes, which nobody can move within one block without holding the
/// price there for the whole window.
library UniswapPrice {
    int24 internal constant MIN_TICK = -887_272;
    int24 internal constant MAX_TICK = 887_272;

    /// The pool's mean tick over the last `window` seconds. Reverts if the
    /// pool's observations don't reach that far back.
    function meanTick(IUniswapV3Pool pool, uint32 window) internal view returns (int24 tick) {
        uint32[] memory ago = new uint32[](2);
        ago[0] = window;
        ago[1] = 0;
        (int56[] memory cumulatives,) = pool.observe(ago);
        int56 delta = cumulatives[1] - cumulatives[0];
        int56 span = int56(uint56(window));
        tick = int24(delta / span);
        // Round towards negative infinity, as the pool does.
        if (delta < 0 && (delta % span != 0)) tick--;
    }

    /// How much of `quote` `amount` of `base` buys at `tick`, where the
    /// pool's price is token1 per token0.
    function quoteAtTick(
        int24 tick,
        uint256 amount,
        address base,
        address quote
    ) internal pure returns (uint256) {
        uint160 sqrtRatio = sqrtRatioAtTick(tick);
        // Two ways round, so the square never overflows.
        if (sqrtRatio <= type(uint128).max) {
            uint256 ratioX192 = uint256(sqrtRatio) * sqrtRatio;
            return base < quote
                ? Math.mulDiv(ratioX192, amount, 1 << 192)
                : Math.mulDiv(1 << 192, amount, ratioX192);
        }
        uint256 ratioX128 = Math.mulDiv(sqrtRatio, sqrtRatio, 1 << 64);
        return
            base < quote ? Math.mulDiv(ratioX128, amount, 1 << 128) : Math.mulDiv(1 << 128, amount, ratioX128);
    }

    /// sqrt(1.0001^tick) as a Q64.96, as the pool computes it.
    function sqrtRatioAtTick(int24 tick) internal pure returns (uint160) {
        uint256 absTick = tick < 0 ? uint256(-int256(tick)) : uint256(int256(tick));
        require(absTick <= uint256(int256(MAX_TICK)), "tick");

        uint256 ratio =
            absTick & 0x1 != 0 ? 0xfffcb933bd6fad37aa2d162d1a594001 : 0x100000000000000000000000000000000;
        if (absTick & 0x2 != 0) ratio = (ratio * 0xfff97272373d413259a46990580e213a) >> 128;
        if (absTick & 0x4 != 0) ratio = (ratio * 0xfff2e50f5f656932ef12357cf3c7fdcc) >> 128;
        if (absTick & 0x8 != 0) ratio = (ratio * 0xffe5caca7e10e4e61c3624eaa0941cd0) >> 128;
        if (absTick & 0x10 != 0) ratio = (ratio * 0xffcb9843d60f6159c9db58835c926644) >> 128;
        if (absTick & 0x20 != 0) ratio = (ratio * 0xff973b41fa98c081472e6896dfb254c0) >> 128;
        if (absTick & 0x40 != 0) ratio = (ratio * 0xff2ea16466c96a3843ec78b326b52861) >> 128;
        if (absTick & 0x80 != 0) ratio = (ratio * 0xfe5dee046a99a2a811c461f1969c3053) >> 128;
        if (absTick & 0x100 != 0) ratio = (ratio * 0xfcbe86c7900a88aedcffc83b479aa3a4) >> 128;
        if (absTick & 0x200 != 0) ratio = (ratio * 0xf987a7253ac413176f2b074cf7815e54) >> 128;
        if (absTick & 0x400 != 0) ratio = (ratio * 0xf3392b0822b70005940c7a398e4b70f3) >> 128;
        if (absTick & 0x800 != 0) ratio = (ratio * 0xe7159475a2c29b7443b29c7fa6e889d9) >> 128;
        if (absTick & 0x1000 != 0) ratio = (ratio * 0xd097f3bdfd2022b8845ad8f792aa5825) >> 128;
        if (absTick & 0x2000 != 0) ratio = (ratio * 0xa9f746462d870fdf8a65dc1f90e061e5) >> 128;
        if (absTick & 0x4000 != 0) ratio = (ratio * 0x70d869a156d2a1b890bb3df62baf32f7) >> 128;
        if (absTick & 0x8000 != 0) ratio = (ratio * 0x31be135f97d08fd981231505542fcfa6) >> 128;
        if (absTick & 0x10000 != 0) ratio = (ratio * 0x9aa508b5b7a84e1c677de54f3e99bc9) >> 128;
        if (absTick & 0x20000 != 0) ratio = (ratio * 0x5d6af8dedb81196699c329225ee604) >> 128;
        if (absTick & 0x40000 != 0) ratio = (ratio * 0x2216e584f5fa1ea926041bedfe98) >> 128;
        if (absTick & 0x80000 != 0) ratio = (ratio * 0x48a170391f7dc42444e8fa2) >> 128;

        if (tick > 0) ratio = type(uint256).max / ratio;
        // From Q128.128 to Q128.96, rounding up.
        return uint160((ratio >> 32) + (ratio % (1 << 32) == 0 ? 0 : 1));
    }
}
