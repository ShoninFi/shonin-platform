//! What selling the station's SOL in the selling pool fetches, worked out
//! step by step as Orca's swap works it out, through the pool's liquidity
//! less the station's own.
//!
//! This is what `exit` values the SOL at. A close withdraws the leaver's
//! part of every position first, then sells; the stayers' liquidity stays in
//! the pool and trades against the leaver's sale, and arbitrage of the
//! difference is theirs. Valued as a sale of all of it through everyone
//! else's liquidity, the station's holdings are worth exactly what the
//! leaver's part fetches plus what the stayers' part would fetch after it
//! (a pool's price follows the same curve however a sale is split), so the
//! leaver is paid no more than their part fetched and the stayers keep
//! theirs. And a fraction, sold through more liquidity than the whole would
//! be, never fetches less than its share of the whole's proceeds.
//!
//! Rounding is the pool's at every step, and against the seller. The sale
//! goes no further than the band's limit, and no further than the tick
//! arrays it was given reach: what is left unsold is valued at the limit,
//! the least a sale inside the band could fetch for it.

use anchor_lang::prelude::*;

use crate::math::{self, amount_a, amount_b, MathError, U256};
use crate::orca::{TickArray, Whirlpool, TICKS_PER_ARRAY};
use crate::wide::W;

/// A position of the station's in the selling pool: its liquidity, which the
/// sale does not trade against, between its ticks.
#[derive(Clone, Copy, Debug)]
pub struct Own {
    pub liquidity: u128,
    pub tick_lower: i32,
    pub tick_upper: i32,
}

/// What a sale fetched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sale {
    /// Of the token sold, how much the sale took, its fees included.
    pub sold: u64,
    /// Of the other token, what it paid.
    pub proceeds: u64,
}

const FEE_ONE: u128 = 1_000_000;

/// Sells `amount` of token A (`a_to_b`) or of token B in `pool`, no further
/// than the square-root price `limit`, through the pool's liquidity less
/// `own`, with the ticks of `arrays` (those a swap would be given).
pub fn sell(
    pool: &Whirlpool,
    arrays: &[TickArray],
    own: &[Own],
    amount: u64,
    a_to_b: bool,
    limit: u128,
) -> Result<Sale> {
    let spacing = pool.tick_spacing as i32;
    let span = TICKS_PER_ARRAY * spacing;
    let fee_rate = pool.fee_rate as u128;
    // The pool's liquidity at its price, and the station's there.
    let mut liquidity = pool_liquidity(pool) as i128;
    let mut price = pool.sqrt_price;
    let mut tick = pool.tick_current;
    let mut remaining = amount;
    let mut proceeds: u64 = 0;
    // How far the given arrays reach in the sale's direction.
    let reach = |a: &TickArray| a.start;
    let (lowest, highest) = arrays.iter().fold((i32::MAX, i32::MIN), |(lo, hi), a| {
        (lo.min(reach(a)), hi.max(reach(a) + span))
    });
    if arrays.is_empty() {
        return Ok(Sale::default());
    }
    for _ in 0..64 {
        if remaining == 0 || price == limit {
            break;
        }
        // The next initialized tick in the sale's direction, or the edge of
        // what the arrays cover.
        let next = next_tick(arrays, tick, spacing, a_to_b)?;
        let (next_tick, initialized) = match next {
            Some(found) => (found, true),
            None => (if a_to_b { lowest } else { highest }, false),
        };
        let tick_price = math::sqrt_price_at(next_tick.clamp(math::MIN_TICK, math::MAX_TICK))
            .map_err(math_error)?;
        let target = if a_to_b {
            tick_price.max(limit)
        } else {
            tick_price.min(limit)
        };
        let theirs = liquidity - own_at(own, tick) as i128;
        require!(theirs >= 0, crate::DriverError::WrongAccounts);
        let step = step(remaining, fee_rate, theirs as u128, price, target, a_to_b)?;
        remaining -= step.consumed;
        proceeds = proceeds.saturating_add(step.out);
        price = step.next;
        if price != tick_price || !initialized {
            if price == tick_price && !initialized {
                // At the edge of the given arrays: nothing further is known.
                break;
            }
            continue;
        }
        // Crossing an initialized tick: the liquidity that starts or ends
        // there.
        let net = tick_net(arrays, next_tick, spacing)?;
        if a_to_b {
            liquidity -= net;
            tick = next_tick - 1;
        } else {
            liquidity += net;
            tick = next_tick;
        }
        require!(liquidity >= 0, crate::DriverError::WrongAccounts);
    }
    Ok(Sale {
        sold: amount - remaining,
        proceeds,
    })
}

/// The pool's active liquidity. (Orca keeps it in the pool's account; the
/// driver reads it with the rest.)
fn pool_liquidity(pool: &Whirlpool) -> u128 {
    pool.liquidity
}

/// The station's own liquidity active at `tick`.
fn own_at(own: &[Own], tick: i32) -> u128 {
    own.iter()
        .filter(|o| o.tick_lower <= tick && tick < o.tick_upper)
        .map(|o| o.liquidity)
        .sum()
}

/// The next initialized tick from `tick` in the sale's direction among
/// `arrays`: at or below it selling A, above it selling B.
fn next_tick(arrays: &[TickArray], tick: i32, spacing: i32, a_to_b: bool) -> Result<Option<i32>> {
    let span = TICKS_PER_ARRAY * spacing;
    // The array holding the tick, and the tick's offset in it.
    let mut start = tick.div_euclid(span) * span;
    let mut offset = (tick - start).div_euclid(spacing);
    loop {
        let Some(array) = arrays.iter().find(|a| a.start == start) else {
            return Ok(None);
        };
        if let Some(found) = array.next_initialized(offset, a_to_b) {
            return Ok(Some(start + found * spacing));
        }
        // On to the next array in the sale's direction.
        if a_to_b {
            start -= span;
            offset = TICKS_PER_ARRAY - 1;
        } else {
            start += span;
            offset = -1;
        }
    }
}

fn tick_net(arrays: &[TickArray], tick: i32, spacing: i32) -> Result<i128> {
    let span = TICKS_PER_ARRAY * spacing;
    let start = tick.div_euclid(span) * span;
    let array = arrays
        .iter()
        .find(|a| a.start == start)
        .ok_or(crate::DriverError::WrongAccounts)?;
    Ok(array.tick(tick, spacing as u16)?.liquidity_net)
}

struct Step {
    consumed: u64,
    out: u64,
    next: u128,
}

/// One step of an exact-input swap between `price` and `target` at
/// `liquidity`, as Orca's program computes it.
fn step(
    remaining: u64,
    fee_rate: u128,
    liquidity: u128,
    price: u128,
    target: u128,
    a_to_b: bool,
) -> Result<Step> {
    if liquidity == 0 {
        // Nothing to trade against: the price moves to the target for free.
        return Ok(Step {
            consumed: 0,
            out: 0,
            next: target,
        });
    }
    let less_fee = math::mul_div_u128(remaining as u128, FEE_ONE - fee_rate, FEE_ONE)
        .ok_or(crate::DriverError::Overflow)? as u64;
    // What reaching the target would take; past what fits 64 bits it is
    // simply more than there is.
    let to_target = if a_to_b {
        amount_a(target, price, liquidity, true)
    } else {
        amount_b(price, target, liquidity, true)
    };
    let reaches = matches!(to_target, Ok(needed) if needed <= less_fee);
    let next = if reaches {
        target
    } else if a_to_b {
        next_price_from_a(price, liquidity, less_fee)?
    } else {
        next_price_from_b(price, liquidity, less_fee)?
    };
    let input = if reaches {
        to_target.map_err(math_error)?
    } else if a_to_b {
        amount_a(next, price, liquidity, true).map_err(math_error)?
    } else {
        amount_b(price, next, liquidity, true).map_err(math_error)?
    };
    let out = if a_to_b {
        amount_b(next, price, liquidity, false)
    } else {
        amount_a(price, next, liquidity, false)
    }
    .map_err(math_error)?;
    let fee = if reaches {
        // The fee on what reaching the target took, rounded up.
        let numerator = input as u128 * fee_rate;
        let denominator = FEE_ONE - fee_rate;
        numerator.div_ceil(denominator) as u64
    } else {
        remaining - input
    };
    Ok(Step {
        consumed: input.saturating_add(fee).min(remaining),
        out,
        next,
    })
}

/// The square-root price after `amount` of A goes in: `L * s / (L + amount *
/// s / 2^64)`, rounded up, as the pool rounds it.
fn next_price_from_a(price: u128, liquidity: u128, amount: u64) -> Result<u128> {
    if amount == 0 {
        return Ok(price);
    }
    let numerator = W::mul(liquidity, price)
        .shl_words(1)
        .ok_or(crate::DriverError::Overflow)?;
    let denominator = U256(W::from_u128(liquidity).shl_words(1).unwrap_or(W::MAX).0)
        + U256(W::mul(price, amount as u128).0);
    let (q, r) = U256(numerator.0).div_mod(denominator);
    let q = if r.is_zero() { q } else { q + 1 };
    require!(q <= U256::from(u128::MAX), crate::DriverError::Overflow);
    Ok(q.low_u128().max(math::MIN_SQRT_PRICE))
}

/// The square-root price after `amount` of B goes in: `s + amount * 2^64 /
/// L`, rounded down.
fn next_price_from_b(price: u128, liquidity: u128, amount: u64) -> Result<u128> {
    let delta = ((amount as u128) << 64) / liquidity;
    price
        .checked_add(delta)
        .ok_or(error!(crate::DriverError::Overflow))
}

fn math_error(_: MathError) -> Error {
    error!(crate::DriverError::Overflow)
}
