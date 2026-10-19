//! A Uniswap v3 pool's price against a feed's, and the swap that brings it
//! there: Uniswap's own tick and amount formulas, in whole numbers, so the
//! amount sent is what the pool would take to reach the price.
//!
//! A pool's price is token1 per token0, in the tokens' smallest units, kept
//! as its square root in Q64.96 (`sqrtPriceX96`). A feed's price is dollars
//! per whole token, with the feed's decimals. Which of the pool's two tokens
//! is the dollar depends on the addresses, so every formula here takes the
//! order.

use alloy::primitives::{U256, U512};

/// The lowest and highest ticks a pool can reach.
pub const MIN_TICK: i32 = -887_272;
pub const MAX_TICK: i32 = 887_272;
/// The square-root prices at those ticks.
pub const MIN_SQRT_RATIO: U256 = U256::from_limbs([4_295_128_739, 0, 0, 0]);
pub const MAX_SQRT_RATIO: U256 = U256::from_limbs([
    0x5d95_1d52_6398_8d26,
    0xefd1_fc6a_5064_8849,
    0x0000_0000_fffd_8963,
    0,
]);

/// The fee's denominator: a pool's fee is in millionths.
const FEE_UNITS: u64 = 1_000_000;

fn q96() -> U256 {
    U256::from(1u8) << 96
}

fn pow10(n: u32) -> U512 {
    U512::from(10u8).pow(U512::from(n))
}

fn narrow(x: U512) -> Option<U256> {
    let limbs = x.as_limbs();
    limbs[4..]
        .iter()
        .all(|limb| *limb == 0)
        .then(|| U256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]]))
}

/// `a * b / c`, rounded down, or `None` if the quotient doesn't fit.
pub fn mul_div(a: U256, b: U256, c: U256) -> Option<U256> {
    if c.is_zero() {
        return None;
    }
    narrow(U512::from(a) * U512::from(b) / U512::from(c))
}

/// `a * b / c`, rounded up.
pub fn mul_div_up(a: U256, b: U256, c: U256) -> Option<U256> {
    if c.is_zero() {
        return None;
    }
    let product = U512::from(a) * U512::from(b);
    let c = U512::from(c);
    let quotient = product / c;
    let quotient = if (product % c).is_zero() {
        quotient
    } else {
        quotient + U512::from(1u8)
    };
    narrow(quotient)
}

/// How a pool's two tokens line up with the feed's: which is the dollar,
/// and each one's decimals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pair {
    /// The listed token is the pool's token0 (and the dollar its token1).
    pub token_is_token0: bool,
    pub token_decimals: u8,
    pub dollar_decimals: u8,
    pub feed_decimals: u8,
}

impl Pair {
    /// The pool's square-root price at the feed's `answer` (dollars per
    /// whole token, in the feed's decimals), rounded down; `None` for a
    /// price that isn't positive or that no pool could hold.
    pub fn sqrt_price_at(&self, answer: U256) -> Option<U256> {
        if answer.is_zero() {
            return None;
        }
        // Dollar units per token unit = answer * 10^dollar / 10^(feed + token).
        let scale = pow10(u32::from(self.feed_decimals) + u32::from(self.token_decimals));
        let dollar = pow10(u32::from(self.dollar_decimals));
        let answer = U512::from(answer);
        let ratio_x192 = if self.token_is_token0 {
            ((answer * dollar) << 192usize) / scale
        } else {
            (scale << 192usize) / (answer * dollar)
        };
        let root = narrow(ratio_x192.root(2))?;
        (MIN_SQRT_RATIO..MAX_SQRT_RATIO)
            .contains(&root)
            .then_some(root)
    }

    /// What `amount` of the token is worth in the dollar's units at the
    /// feed's `answer`.
    pub fn dollars_for(&self, amount: U256, answer: U256) -> U256 {
        let scale = U256::from(10u8).pow(U256::from(
            u32::from(self.feed_decimals) + u32::from(self.token_decimals)
                - u32::from(self.dollar_decimals),
        ));
        mul_div(amount, answer, scale).unwrap_or(U256::MAX)
    }

    /// How much of the token `dollars` (in the dollar's units) buys at the
    /// feed's `answer`.
    pub fn tokens_for(&self, dollars: U256, answer: U256) -> U256 {
        let scale = U256::from(10u8).pow(U256::from(
            u32::from(self.feed_decimals) + u32::from(self.token_decimals)
                - u32::from(self.dollar_decimals),
        ));
        mul_div(dollars, scale, answer).unwrap_or(U256::MAX)
    }

    /// Whether a swap that lowers the pool's price (token0 in) spends the
    /// dollar.
    pub fn zero_for_one_spends_dollar(&self) -> bool {
        !self.token_is_token0
    }
}

/// How far the pool's price is from the target's, in basis points of the
/// target's: positive when the pool's token1-per-token0 price is above it.
pub fn deviation_bps(sqrt_price: U256, target: U256) -> f64 {
    let ratio = to_f64(sqrt_price) / to_f64(target);
    (ratio * ratio - 1.0) * 10_000.0
}

/// The listed token's price in the pool against the feed's, in basis
/// points: positive when the pool prices the token above the feed.
pub fn token_premium_bps(pair: &Pair, sqrt_price: U256, target: U256) -> f64 {
    let deviation = deviation_bps(sqrt_price, target);
    if pair.token_is_token0 {
        deviation
    } else {
        // token1 per token0 is tokens per dollar here: a higher pool price
        // is a cheaper token.
        let ratio = 1.0 + deviation / 10_000.0;
        (1.0 / ratio - 1.0) * 10_000.0
    }
}

/// A number as a float, near enough to compare prices.
pub fn to_f64(x: U256) -> f64 {
    let bits = x.bit_len();
    if bits <= 64 {
        x.to::<u64>() as f64
    } else {
        let shift = bits - 64;
        (x >> shift).to::<u64>() as f64 * 2f64.powi(shift as i32)
    }
}

/// sqrt(1.0001^tick) in Q64.96, exactly as the pool computes it.
pub fn sqrt_ratio_at_tick(tick: i32) -> U256 {
    assert!(
        (MIN_TICK..=MAX_TICK).contains(&tick),
        "tick {tick} out of range"
    );
    let abs = tick.unsigned_abs();
    const FACTORS: [u128; 19] = [
        0xfff97272373d413259a46990580e213a,
        0xfff2e50f5f656932ef12357cf3c7fdcc,
        0xffe5caca7e10e4e61c3624eaa0941cd0,
        0xffcb9843d60f6159c9db58835c926644,
        0xff973b41fa98c081472e6896dfb254c0,
        0xff2ea16466c96a3843ec78b326b52861,
        0xfe5dee046a99a2a811c461f1969c3053,
        0xfcbe86c7900a88aedcffc83b479aa3a4,
        0xf987a7253ac413176f2b074cf7815e54,
        0xf3392b0822b70005940c7a398e4b70f3,
        0xe7159475a2c29b7443b29c7fa6e889d9,
        0xd097f3bdfd2022b8845ad8f792aa5825,
        0xa9f746462d870fdf8a65dc1f90e061e5,
        0x70d869a156d2a1b890bb3df62baf32f7,
        0x31be135f97d08fd981231505542fcfa6,
        0x09aa508b5b7a84e1c677de54f3e99bc9,
        0x005d6af8dedb81196699c329225ee604,
        0x00002216e584f5fa1ea926041bedfe98,
        0x0000000048a170391f7dc42444e8fa2,
    ];
    let mut ratio = if abs & 1 != 0 {
        U256::from(0xfffcb933bd6fad37aa2d162d1a594001u128)
    } else {
        U256::from(1u8) << 128
    };
    for (bit, factor) in FACTORS.iter().enumerate() {
        if abs & (2 << bit) != 0 {
            ratio = (ratio * U256::from(*factor)) >> 128;
        }
    }
    if tick > 0 {
        ratio = U256::MAX / ratio;
    }
    // Q128.128 to Q64.96, rounding up.
    let rounded = if (ratio % (U256::from(1u8) << 32usize)).is_zero() {
        0u8
    } else {
        1u8
    };
    (ratio >> 32) + U256::from(rounded)
}

/// The greatest tick whose square-root price is at most `sqrt_price`.
pub fn tick_at_sqrt_ratio(sqrt_price: U256) -> i32 {
    let price = (to_f64(sqrt_price) / 2f64.powi(96)).powi(2);
    let mut tick = (price.ln() / 1.0001f64.ln()).floor() as i32;
    tick = tick.clamp(MIN_TICK, MAX_TICK - 1);
    while tick > MIN_TICK && sqrt_ratio_at_tick(tick) > sqrt_price {
        tick -= 1;
    }
    while tick < MAX_TICK - 1 && sqrt_ratio_at_tick(tick + 1) <= sqrt_price {
        tick += 1;
    }
    tick
}

/// The token0 that moves the price between two square roots at liquidity
/// `liquidity`: Uniswap's `getAmount0Delta`.
pub fn amount0_delta(a: U256, b: U256, liquidity: u128, round_up: bool) -> U256 {
    let (lower, upper) = if a < b { (a, b) } else { (b, a) };
    if lower.is_zero() {
        return U256::MAX;
    }
    let numerator1 = U256::from(liquidity) << 96;
    let numerator2 = upper - lower;
    if round_up {
        let inner = mul_div_up(numerator1, numerator2, upper).unwrap_or(U256::MAX);
        inner.div_ceil(lower)
    } else {
        mul_div(numerator1, numerator2, upper).unwrap_or(U256::MAX) / lower
    }
}

/// The token1 that moves the price between two square roots: Uniswap's
/// `getAmount1Delta`.
pub fn amount1_delta(a: U256, b: U256, liquidity: u128, round_up: bool) -> U256 {
    let (lower, upper) = if a < b { (a, b) } else { (b, a) };
    let liquidity = U256::from(liquidity);
    if round_up {
        mul_div_up(liquidity, upper - lower, q96()).unwrap_or(U256::MAX)
    } else {
        mul_div(liquidity, upper - lower, q96()).unwrap_or(U256::MAX)
    }
}

/// A pool as read: its square-root price, its tick, the liquidity in range,
/// and its fee in millionths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolState {
    pub sqrt_price: U256,
    pub tick: i32,
    pub liquidity: u128,
    pub fee: u32,
}

/// The swap that moves a pool to a target price.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// token0 in (the price falls), or token1 in (it rises).
    pub zero_for_one: bool,
    /// What the pool takes, its fee included, to reach the target (or as
    /// far as the ticks read reach).
    pub amount_in: U256,
    /// What comes out of the pool for it.
    pub amount_out: U256,
    /// The ticks read reach the target: the amount is all it takes.
    pub reaches: bool,
}

/// The swap that takes `pool` to `target`, crossing the initialised ticks
/// `crossed` (each with its net liquidity, in the order the price meets
/// them) and nothing past the last of them that was read up to `read_to`.
/// `None` when the pool is at the target.
pub fn plan_to(
    pool: &PoolState,
    target: U256,
    crossed: &[(i32, i128)],
    read_to: Option<i32>,
) -> Option<Plan> {
    if target == pool.sqrt_price {
        return None;
    }
    let zero_for_one = target < pool.sqrt_price;
    // How far the ticks read let the walk go.
    let mut end = target;
    if let Some(limit) = read_to {
        let at = sqrt_ratio_at_tick(limit.clamp(MIN_TICK, MAX_TICK));
        if zero_for_one && at > end {
            end = at;
        }
        if !zero_for_one && at < end {
            end = at;
        }
    }
    let mut price = pool.sqrt_price;
    let mut liquidity = i128::try_from(pool.liquidity).unwrap_or(i128::MAX);
    let mut amount_in = U256::ZERO;
    let mut amount_out = U256::ZERO;
    let mut segment = |from: U256, to: U256, liquidity: i128| {
        let liquidity = u128::try_from(liquidity.max(0)).unwrap_or(0);
        if liquidity == 0 || from == to {
            return;
        }
        let (gross_in, out) = if zero_for_one {
            (
                amount0_delta(to, from, liquidity, true),
                amount1_delta(to, from, liquidity, false),
            )
        } else {
            (
                amount1_delta(from, to, liquidity, true),
                amount0_delta(from, to, liquidity, false),
            )
        };
        // The fee is taken on top of what moves the price.
        let fee = U256::from(pool.fee);
        let with_fee = mul_div_up(gross_in, U256::from(FEE_UNITS), U256::from(FEE_UNITS) - fee)
            .unwrap_or(U256::MAX);
        amount_in = amount_in.saturating_add(with_fee);
        amount_out = amount_out.saturating_add(out);
    };
    for &(tick, net) in crossed {
        let at = sqrt_ratio_at_tick(tick);
        let beyond = if zero_for_one { at <= end } else { at >= end };
        if beyond {
            break;
        }
        segment(price, at, liquidity);
        price = at;
        // Crossing a tick downwards takes its net liquidity out; upwards
        // puts it in.
        liquidity = if zero_for_one {
            liquidity.saturating_sub(net)
        } else {
            liquidity.saturating_add(net)
        };
    }
    segment(price, end, liquidity);
    Some(Plan {
        zero_for_one,
        amount_in,
        amount_out,
        reaches: end == target,
    })
}

/// The initialised ticks a tick bitmap word holds: word `word` of a pool
/// whose ticks are `spacing` apart.
pub fn ticks_in_word(word: i16, bits: U256, spacing: i32) -> Vec<i32> {
    (0..256)
        .filter(|bit| bits.bit(*bit))
        .map(|bit| ((i32::from(word) << 8) + bit as i32) * spacing)
        .collect()
}

/// The bitmap word holding `tick` at `spacing`.
pub fn word_of(tick: i32, spacing: i32) -> i16 {
    let compressed = tick.div_euclid(spacing);
    (compressed >> 8) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(x: u128) -> U256 {
        U256::from(x)
    }

    #[test]
    fn the_tick_formula_matches_uniswap_s_at_its_ends_and_at_zero() {
        assert_eq!(sqrt_ratio_at_tick(MIN_TICK), MIN_SQRT_RATIO);
        assert_eq!(sqrt_ratio_at_tick(MAX_TICK), MAX_SQRT_RATIO);
        assert_eq!(sqrt_ratio_at_tick(0), q96());
        // Two pools as read from the chain, one each side of zero: each
        // pool's price lies between its tick's and the next one's.
        for (tick, sqrt_price) in [
            (217_286, "4139677031943631945530809493028000"),
            (-223_680, "1101485183252237211607764"),
        ] {
            let sqrt_price: U256 = sqrt_price.parse().unwrap();
            assert!(sqrt_ratio_at_tick(tick) <= sqrt_price);
            assert!(sqrt_price < sqrt_ratio_at_tick(tick + 1));
        }
    }

    #[test]
    fn a_tick_is_found_back_from_its_own_price_and_from_just_above_it() {
        for tick in [-887_000, -223_680, -50, -1, 0, 1, 217_286, 887_000] {
            let at = sqrt_ratio_at_tick(tick);
            assert_eq!(tick_at_sqrt_ratio(at), tick);
            assert_eq!(tick_at_sqrt_ratio(at + d(1)), tick);
            assert_eq!(tick_at_sqrt_ratio(at - d(1)), tick - 1);
        }
    }

    /// A stock at $400 against a 6-decimal dollar, 18-decimal token, 8-decimal
    /// feed, in either order.
    fn pair(token_is_token0: bool) -> Pair {
        Pair {
            token_is_token0,
            token_decimals: 18,
            dollar_decimals: 6,
            feed_decimals: 8,
        }
    }

    #[test]
    fn the_target_price_is_the_feed_s_price_in_the_pool_s_units_in_either_order() {
        let answer = d(400_00000000);
        // token0 = token: 400e6 dollar units per 1e18 token units.
        let sqrt = pair(true).sqrt_price_at(answer).unwrap();
        let price = (to_f64(sqrt) / 2f64.powi(96)).powi(2);
        assert!((price / 4e-10 - 1.0).abs() < 1e-12, "{price}");
        // token0 = dollar: 1e18 / 400e6 token units per dollar unit.
        let sqrt = pair(false).sqrt_price_at(answer).unwrap();
        let price = (to_f64(sqrt) / 2f64.powi(96)).powi(2);
        assert!((price / 2.5e9 - 1.0).abs() < 1e-12, "{price}");
        assert_eq!(pair(true).sqrt_price_at(U256::ZERO), None);
    }

    #[test]
    fn the_targets_of_the_two_orders_are_reciprocal() {
        let answer = d(239_790_849_646);
        let a = to_f64(pair(true).sqrt_price_at(answer).unwrap()) / 2f64.powi(96);
        let b = to_f64(pair(false).sqrt_price_at(answer).unwrap()) / 2f64.powi(96);
        assert!((a * b - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_pool_s_premium_is_read_the_same_whichever_token_is_first() {
        let answer = d(400_00000000);
        let ten_up = d(440_00000000);
        for order in [true, false] {
            let p = pair(order);
            let target = p.sqrt_price_at(answer).unwrap();
            let pool = p.sqrt_price_at(ten_up).unwrap();
            let premium = token_premium_bps(&p, pool, target);
            assert!((premium - 1_000.0).abs() < 0.01, "{order}: {premium}");
        }
    }

    #[test]
    fn dollars_and_tokens_convert_at_the_feed_with_the_decimals_mixed() {
        let p = pair(true);
        let answer = d(400_00000000);
        assert_eq!(
            p.dollars_for(d(2_500_000_000_000_000_000), answer),
            d(1_000_000_000)
        );
        assert_eq!(
            p.tokens_for(d(1_000_000_000), answer),
            d(2_500_000_000_000_000_000)
        );
        // An 18-decimal token priced by an 8-decimal feed against an
        // 18-decimal dollar.
        let wide = Pair {
            dollar_decimals: 18,
            ..p
        };
        assert_eq!(
            wide.dollars_for(d(1_000_000_000_000_000_000), answer),
            d(400_000_000_000_000_000_000)
        );
    }

    /// One position across the whole range: no tick is met between.
    fn full_range(sqrt_price: U256, liquidity: u128) -> PoolState {
        PoolState {
            sqrt_price,
            tick: tick_at_sqrt_ratio(sqrt_price),
            liquidity,
            fee: 500,
        }
    }

    #[test]
    fn the_swap_to_a_lower_price_spends_token0_and_to_a_higher_one_token1() {
        let p = pair(true);
        let at = p.sqrt_price_at(d(400_00000000)).unwrap();
        let pool = full_range(at, 1_000_000_000_000_000);
        let down = plan_to(&pool, p.sqrt_price_at(d(380_00000000)).unwrap(), &[], None).unwrap();
        assert!(down.zero_for_one);
        assert!(down.reaches);
        let up = plan_to(&pool, p.sqrt_price_at(d(420_00000000)).unwrap(), &[], None).unwrap();
        assert!(!up.zero_for_one);
        assert_eq!(plan_to(&pool, at, &[], None), None);
    }

    #[test]
    fn the_amount_in_a_full_range_pool_is_uniswap_s_delta_with_its_fee() {
        // L = sqrt(x * y): a pool of 5 tokens and 2,000 dollars at $400.
        let p = pair(true);
        let at = p.sqrt_price_at(d(400_00000000)).unwrap();
        let liquidity: u128 = 100_000_000_000_000; // sqrt(5e18 * 2e9)
        let pool = full_range(at, liquidity);
        let target = p.sqrt_price_at(d(441_00000000)).unwrap();
        let plan = plan_to(&pool, target, &[], None).unwrap();
        // Price up 10.25% is sqrt up 5%: token1 in = L * (sqrt_t - sqrt_c).
        let gross = amount1_delta(at, target, liquidity, true);
        assert_eq!(
            plan.amount_in,
            mul_div_up(gross, d(1_000_000), d(999_500)).unwrap()
        );
        // About 5% of the 2,000 dollars, a little over a hundred dollars.
        let dollars = to_f64(plan.amount_in) / 1e6;
        assert!((100.0..101.0).contains(&dollars), "{dollars}");
        // And about 4.76% of the tokens come out.
        let tokens = to_f64(plan.amount_out) / 1e18;
        assert!((0.237..0.239).contains(&tokens), "{tokens}");
    }

    #[test]
    fn crossing_a_tick_changes_the_liquidity_the_rest_of_the_swap_meets() {
        let p = pair(true);
        let at = p.sqrt_price_at(d(400_00000000)).unwrap();
        let target = p.sqrt_price_at(d(441_00000000)).unwrap();
        let pool = full_range(at, 1_000_000_000_000);
        let middle = tick_at_sqrt_ratio(p.sqrt_price_at(d(420_00000000)).unwrap());
        let flat = plan_to(&pool, target, &[], None).unwrap();
        // Upwards, a tick whose net liquidity is positive adds liquidity:
        // more is needed past it.
        let deeper = plan_to(&pool, target, &[(middle, 1_000_000_000_000)], None).unwrap();
        assert!(deeper.amount_in > flat.amount_in);
        let before = amount1_delta(at, sqrt_ratio_at_tick(middle), 1_000_000_000_000, true);
        let after = amount1_delta(sqrt_ratio_at_tick(middle), target, 2_000_000_000_000, true);
        let expected = mul_div_up(before, d(1_000_000), d(999_500)).unwrap()
            + mul_div_up(after, d(1_000_000), d(999_500)).unwrap();
        assert_eq!(deeper.amount_in, expected);
        // A tick past the target is never met.
        let beyond = tick_at_sqrt_ratio(target) + 100;
        assert_eq!(plan_to(&pool, target, &[(beyond, 5)], None).unwrap(), flat);
    }

    #[test]
    fn a_walk_cut_short_by_the_ticks_read_says_it_does_not_reach() {
        let p = pair(false);
        let at = p.sqrt_price_at(d(401_800_000_000)).unwrap();
        let target = p.sqrt_price_at(d(239_700_000_000)).unwrap();
        let pool = full_range(at, 448_000_000_000);
        // The dollar is token0 here: the token falling is the pool's price
        // rising, so the token (token1) goes in.
        let whole = plan_to(&pool, target, &[], None).unwrap();
        assert!(!whole.zero_for_one);
        assert!(whole.reaches);
        let halfway = (pool.tick + tick_at_sqrt_ratio(target)) / 2;
        let cut = plan_to(&pool, target, &[], Some(halfway)).unwrap();
        assert!(!cut.reaches);
        assert!(cut.amount_in < whole.amount_in);
        // From $4,018 to $2,397 at that liquidity is about 0.002 WETH.
        let weth = to_f64(whole.amount_in) / 1e18;
        assert!((0.0019..0.0022).contains(&weth), "{weth}");
    }

    #[test]
    fn bitmap_words_name_their_ticks_with_negative_words_below_zero() {
        let mut bits = U256::ZERO;
        bits.set_bit(0, true);
        bits.set_bit(255, true);
        assert_eq!(ticks_in_word(0, bits, 10), vec![0, 2_550]);
        assert_eq!(ticks_in_word(-1, bits, 10), vec![-2_560, -10]);
        assert_eq!(word_of(-10, 10), -1);
        assert_eq!(word_of(-2_560, 10), -1);
        assert_eq!(word_of(-2_561, 10), -2);
        assert_eq!(word_of(2_550, 10), 0);
        assert_eq!(word_of(MIN_TICK, 1), -3_466);
    }
}
