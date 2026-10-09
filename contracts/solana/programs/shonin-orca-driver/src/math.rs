//! The arithmetic of a concentrated-liquidity pool, exact to the unit.
//!
//! A pool's price is kept as its square root in Q64.64 (`sqrt_price`, units
//! of token B per unit of token A, square-rooted, times 2^64), and a tick `i`
//! stands for the price `1.0001^i`. A position of liquidity `L` between the
//! square-root prices `lower` and `upper` holds, at the square-root price
//! `s` inside its range,
//!
//! ```text
//! a = L * (upper - s) * 2^64 / (s * upper)      (token A)
//! b = L * (s - lower) / 2^64                     (token B)
//! ```
//!
//! and only A below its range, only B above it. Each figure here is worked
//! out exactly as the pool's program works it out (the same rounding at each
//! step), so that what a driver expects a withdrawal to pay is what it pays.
//! Wherever a figure could be rounded either way for a valuation, it is
//! rounded against whoever is paid from it.

use uint::construct_uint;

use crate::wide::W;

#[allow(clippy::manual_div_ceil, clippy::assign_op_pattern)]
mod u256 {
    use super::construct_uint;

    construct_uint! {
        /// 256 bits, for the divisions a word at a time can't do.
        pub struct U256(4);
    }
}
pub use u256::U256;

/// The lowest and highest ticks a pool's price may stand at.
pub const MIN_TICK: i32 = -443_636;
pub const MAX_TICK: i32 = 443_636;
/// The square-root prices of those ticks, the bounds of every square-root
/// price a pool accepts.
pub const MIN_SQRT_PRICE: u128 = 4_295_048_016;
pub const MAX_SQRT_PRICE: u128 = 79_226_673_515_401_279_992_447_579_055;

/// `sqrt(1.0001)^(2^k)` for `k = 1..=18`, in Q32.96: what a positive tick's
/// bit `k` multiplies the square-root price by.
const UP: [u128; 18] = [
    79236085330515764027303304731,
    79244008939048815603706035061,
    79259858533276714757314932305,
    79291567232598584799939703904,
    79355022692464371645785046466,
    79482085999252804386437311141,
    79736823300114093921829183326,
    80248749790819932309965073892,
    81282483887344747381513967011,
    83390072131320151908154831281,
    87770609709833776024991924138,
    97234110755111693312479820773,
    119332217159966728226237229890,
    179736315981702064433883588727,
    407748233172238350107850275304,
    2098478828474011932436660412517,
    55581415166113811149459800483533,
    38992368544603139932233054999993551,
];

/// `sqrt(1.0001)^(-2^k)` for `k = 1..=18`, in Q64.64: what a negative tick's
/// bit `k` multiplies the square-root price by.
const DOWN: [u128; 18] = [
    18444899583751176498,
    18443055278223354162,
    18439367220385604838,
    18431993317065449817,
    18417254355718160513,
    18387811781193591352,
    18329067761203520168,
    18212142134806087854,
    17980523815641551639,
    17526086738831147013,
    16651378430235024244,
    15030750278693429944,
    12247334978882834399,
    8131365268884726200,
    3584323654723342297,
    696457651847595233,
    26294789957452057,
    37481735321082,
];

/// Why a figure can't be worked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathError {
    /// The tick is outside the pool's range.
    Tick,
    /// The figure doesn't fit what holds it.
    Overflow,
}

/// The square-root price of `tick`, in Q64.64, as the pool's program has
/// it.
pub fn sqrt_price_at(tick: i32) -> Result<u128, MathError> {
    if !(MIN_TICK..=MAX_TICK).contains(&tick) {
        return Err(MathError::Tick);
    }
    let bits = tick.unsigned_abs();
    if tick >= 0 {
        // Q32.96 throughout, then down to Q64.64.
        let mut ratio = if bits & 1 != 0 {
            U256::from(79232123823359799118286999567u128)
        } else {
            U256::from(79228162514264337593543950336u128)
        };
        for (k, factor) in UP.iter().enumerate() {
            if bits & (2 << k) != 0 {
                ratio = (ratio * U256::from(*factor)) >> 96;
            }
        }
        Ok((ratio >> 32).low_u128())
    } else {
        let mut ratio: u128 = if bits & 1 != 0 {
            18445821805675392311
        } else {
            18446744073709551616
        };
        for (k, factor) in DOWN.iter().enumerate() {
            if bits & (2 << k) != 0 {
                ratio = (ratio * factor) >> 64;
            }
        }
        Ok(ratio)
    }
}

/// Token A between two square-root prices for liquidity `liquidity`:
/// `L * (upper - lower) * 2^64 / (upper * lower)`, rounded up or down.
pub fn amount_a(s0: u128, s1: u128, liquidity: u128, round_up: bool) -> Result<u64, MathError> {
    let (lower, upper) = if s0 > s1 { (s1, s0) } else { (s0, s1) };
    if lower == 0 {
        return Err(MathError::Overflow);
    }
    let product = W::mul(liquidity, upper - lower);
    // The pool refuses a numerator that doesn't fit 256 bits once shifted.
    let numerator = product.shl_words(1).ok_or(MathError::Overflow)?;
    let (quotient, exact) = if upper <= u64::MAX as u128 {
        // Both prices fit a word (a pool pricing B below A, as SOL against a
        // dollar is): `floor(floor(n / upper) / lower)` is `floor(n / (upper
        // * lower))`, worked a word at a time, and exact all the same.
        let (q, r1) = numerator.div_word(upper as u64);
        let (q, r2) = q.div_word(lower as u64);
        if q.0[1..] != [0; 3] {
            return Err(MathError::Overflow);
        }
        (q.0[0], r1 == 0 && r2 == 0)
    } else {
        let (q, r) = U256(numerator.0).div_mod(U256::from(upper) * U256::from(lower));
        if q > U256::from(u64::MAX) {
            return Err(MathError::Overflow);
        }
        (q.low_u64(), r.is_zero())
    };
    if round_up && !exact {
        return quotient.checked_add(1).ok_or(MathError::Overflow);
    }
    Ok(quotient)
}

/// Token B between two square-root prices for liquidity `liquidity`:
/// `L * (upper - lower) / 2^64`, rounded up or down; the product must fit
/// 128 bits, as the pool requires.
pub fn amount_b(s0: u128, s1: u128, liquidity: u128, round_up: bool) -> Result<u64, MathError> {
    let (lower, upper) = if s0 > s1 { (s1, s0) } else { (s0, s1) };
    let diff = upper - lower;
    if liquidity == 0 || diff == 0 {
        return Ok(0);
    }
    let product = liquidity.checked_mul(diff).ok_or(MathError::Overflow)?;
    let whole = (product >> 64) as u64;
    if round_up && product & u64::MAX as u128 != 0 {
        return whole.checked_add(1).ok_or(MathError::Overflow);
    }
    Ok(whole)
}

/// What a position of `liquidity` between `lower` and `upper` (square-root
/// prices of its ticks) holds of A and B at the square-root price `price`,
/// rounded down: what withdrawing all of it pays.
pub fn amounts_at(
    price: u128,
    lower: u128,
    upper: u128,
    liquidity: u128,
) -> Result<(u64, u64), MathError> {
    if liquidity == 0 {
        return Ok((0, 0));
    }
    if price <= lower {
        Ok((amount_a(lower, upper, liquidity, false)?, 0))
    } else if price < upper {
        Ok((
            amount_a(price, upper, liquidity, false)?,
            amount_b(lower, price, liquidity, false)?,
        ))
    } else {
        Ok((0, amount_b(lower, upper, liquidity, false)?))
    }
}

/// What withdrawing `liquidity` pays, exactly as the pool pays it: which
/// tokens it pays is decided by the pool's current tick against the
/// position's ticks, and the amounts by its current square-root price.
pub fn withdrawal(
    current_tick: i32,
    price: u128,
    tick_lower: i32,
    tick_upper: i32,
    liquidity: u128,
) -> Result<(u64, u64), MathError> {
    if liquidity == 0 {
        return Ok((0, 0));
    }
    let ticks = (tick_lower, tick_upper);
    let roots = (sqrt_price_at(tick_lower)?, sqrt_price_at(tick_upper)?);
    withdrawal_between(current_tick, price, ticks, roots, liquidity)
}

/// The same, given the square-root prices of the position's ticks.
pub fn withdrawal_between(
    current_tick: i32,
    price: u128,
    (tick_lower, tick_upper): (i32, i32),
    (lower, upper): (u128, u128),
    liquidity: u128,
) -> Result<(u64, u64), MathError> {
    if liquidity == 0 {
        return Ok((0, 0));
    }
    if current_tick < tick_lower {
        Ok((amount_a(lower, upper, liquidity, false)?, 0))
    } else if current_tick < tick_upper {
        Ok((
            amount_a(price, upper, liquidity, false)?,
            amount_b(lower, price, liquidity, false)?,
        ))
    } else {
        Ok((0, amount_b(lower, upper, liquidity, false)?))
    }
}

/// The fees a position of `liquidity` has earned since its checkpoint, as
/// the pool's program credits them: `L * (growth - checkpoint) / 2^64`, the
/// growth wrapping, and nothing at all if the product overflows 128 bits.
pub fn fees_earned(liquidity: u128, growth_inside: u128, checkpoint: u128) -> u64 {
    let delta = growth_inside.wrapping_sub(checkpoint);
    match liquidity.checked_mul(delta) {
        Some(product) => (product >> 64) as u64,
        None => 0,
    }
}

/// `value * numerator / denominator`, rounded down, in 256 bits; `None` if
/// the result doesn't fit 64 bits.
pub fn mul_div_u64(value: u64, numerator: u128, denominator: u128) -> Option<u64> {
    if denominator == 0 {
        return None;
    }
    let result = U256::from(value) * U256::from(numerator) / U256::from(denominator);
    (result <= U256::from(u64::MAX)).then(|| result.low_u64())
}

/// `value * numerator / denominator` for a 128-bit value, rounded down.
pub fn mul_div_u128(value: u128, numerator: u128, denominator: u128) -> Option<u128> {
    if denominator == 0 {
        return None;
    }
    // A word at a time when the denominator fits one, as `WAD` does.
    if denominator <= u64::MAX as u128 {
        let (q, _) = W::mul(value, numerator).div_word(denominator as u64);
        return q.to_u128();
    }
    let result = U256::from(value) * U256::from(numerator) / U256::from(denominator);
    (result <= U256::from(u128::MAX)).then(|| result.low_u128())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The square-root price at a tick, worked out in floating point.
    fn approx(tick: i32) -> f64 {
        1.0001f64.powf(tick as f64 / 2.0) * 2f64.powi(64)
    }

    #[test]
    fn the_price_at_tick_zero_is_one() {
        assert_eq!(sqrt_price_at(0), Ok(1u128 << 64));
    }

    #[test]
    fn the_price_at_the_outermost_ticks_is_the_pool_s_bounds() {
        assert_eq!(sqrt_price_at(MIN_TICK), Ok(MIN_SQRT_PRICE));
        assert_eq!(sqrt_price_at(MAX_TICK), Ok(MAX_SQRT_PRICE));
        assert_eq!(sqrt_price_at(MAX_TICK + 1), Err(MathError::Tick));
        assert_eq!(sqrt_price_at(MIN_TICK - 1), Err(MathError::Tick));
    }

    #[test]
    fn the_price_at_a_tick_is_one_point_0001_to_its_half() {
        for tick in [-400_000, -38_054, -1, 1, 64, 5_632, 70_000, 300_000] {
            let exact = sqrt_price_at(tick).unwrap() as f64;
            let float = approx(tick);
            assert!(((exact - float) / float).abs() < 1e-9, "tick {tick}");
        }
    }

    #[test]
    fn the_price_rises_with_every_tick() {
        let mut last = 0;
        for tick in (-5_000..5_000).step_by(7) {
            let price = sqrt_price_at(tick).unwrap();
            assert!(price > last, "tick {tick}");
            last = price;
        }
    }

    #[test]
    fn a_withdrawal_pays_the_tokens_of_the_side_the_price_is_on() {
        let (lower, upper) = (-1_000, 1_000);
        let liquidity = 1_000_000_000_000u128;
        let below = withdrawal(
            -2_000,
            sqrt_price_at(-2_000).unwrap(),
            lower,
            upper,
            liquidity,
        );
        assert!(matches!(below, Ok((a, 0)) if a > 0));
        let above = withdrawal(
            2_000,
            sqrt_price_at(2_000).unwrap(),
            lower,
            upper,
            liquidity,
        );
        assert!(matches!(above, Ok((0, b)) if b > 0));
        let (a, b) = withdrawal(0, 1 << 64, lower, upper, liquidity).unwrap();
        // At a price of one, inside a symmetric range, about as much of each.
        assert!(a.abs_diff(b) <= 1, "{a} {b}");
    }

    #[test]
    fn amounts_round_up_only_when_asked() {
        let (s0, s1, l) = (1u128 << 64, (1u128 << 64) + 12_345, 999_999_937u128);
        let down = amount_a(s0, s1, l, false).unwrap();
        let up = amount_a(s0, s1, l, true).unwrap();
        assert!(up == down || up == down + 1);
        let down = amount_b(s0, s1, l, false).unwrap();
        let up = amount_b(s0, s1, l, true).unwrap();
        assert_eq!(up, down + 1);
    }

    #[test]
    fn fees_earned_wrap_like_the_pool_s() {
        assert_eq!(fees_earned(1 << 64, 10, 3), 7);
        // The growth wraps past zero.
        assert_eq!(fees_earned(1 << 64, 2, u128::MAX), 3);
        // An overflowing product earns nothing, as the pool credits it.
        assert_eq!(fees_earned(u128::MAX, 2, 0), 0);
    }

    #[test]
    fn token_a_is_the_same_by_either_division() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..2_000 {
            let lower = sqrt_price_at(-((next() % 200_000) as i32)).unwrap();
            let upper = lower + (next() as u128 % lower).max(1);
            let upper = upper.min(u64::MAX as u128);
            if upper <= lower {
                continue;
            }
            let liquidity = (next() as u128) << (next() % 40);
            for round_up in [false, true] {
                let fast = amount_a(lower, upper, liquidity, round_up);
                // The full division, as the pool's program does it.
                let product = U256::from(liquidity) * U256::from(upper - lower);
                let (q, r) = (product << 64).div_mod(U256::from(upper) * U256::from(lower));
                let q = if round_up && !r.is_zero() { q + 1 } else { q };
                let full = if q > U256::from(u64::MAX) {
                    Err(MathError::Overflow)
                } else {
                    Ok(q.low_u64())
                };
                assert_eq!(fast, full);
            }
        }
    }
}
