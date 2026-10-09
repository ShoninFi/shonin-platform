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
//! Ticks too small to count (see [`THETA`]) are never stepped through: their
//! removals are made from the start and their additions left out, so the
//! sale works with no more liquidity than the pool has anywhere on its way
//! and fetches no more than Orca's swap would, the stayers keeping any
//! difference.
//!
//! Rounding is the pool's at every step, and against the seller. The sale
//! goes no further than the band's limit, and no further than the tick
//! arrays it was given reach (an array not there yet holds no ticks): what
//! is left unsold fetched nothing, and is worth nothing to `exit`.

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
    /// Where it stopped short of the next exact tick, having crossed all it
    /// may: a swap made no further than this crosses no more of them.
    pub stop: Option<u128>,
    /// The square-root price it ended at.
    pub end: u128,
}

const FEE_ONE: u128 = 1_000_000;

/// The most ticks at least the threshold ([`THETA`]) a close's sale may
/// cross: the sale of everything is worked out twice in a claim's close
/// (before, and for the stayers' part after), about 4,000 units a crossing
/// each. A sale that would cross more stops just short of the next.
pub const CLOSE_CROSSINGS: u32 = 7;

/// The most ticks at least the threshold a valuation's sale may cross:
/// about 4,000 units each on a valuation's 30,000 to 55,000. From a pool at
/// the top of the widest band a station may be created with (10%) to the
/// sale's limit at its bottom is about 32 spacings of 64 ticks; this covers
/// that with room.
pub const VALUE_CROSSINGS: u32 = 40;

/// The least change of liquidity a tick must make to be crossed exactly
/// and counted against a sale's crossings: `1 / THETA` of the most liquidity
/// active anywhere between the pool's price and the sale's limit.
///
/// Anyone can initialize a tick with a sliver of liquidity for the rent of a
/// position, so a sale must never be held to a number of initialized ticks
/// alone. A tick that changes the liquidity by less than this is not crossed
/// at all: where crossing it would remove liquidity, that removal is made
/// from the start of the sale; where it would add liquidity, it is left out.
/// The liquidity the sale works with is then never more than the pool's at
/// any price on its way: it is the pool's as the exact ticks change it,
/// less every small removal on the way to the limit (those not yet crossed
/// included) and without any small addition, never below nothing; so the
/// sale never fetches more than Orca's swap does, a unit of the asset sold
/// through less liquidity moving the price further and being paid less.
///
/// A hundredth: on the mainnet pool, whose active liquidity across one
/// spacing holds about $42,000 of each token, a tick counted against the
/// crossings must hold about $420 at the price the sale would reach, rather
/// than a position's rent; a real provider's tick that big is crossed
/// exactly, and one smaller costs the sale at most a hundredth of the
/// liquidity it would have had beyond it, the stayers keeping the difference.
///
/// The most liquidity on the way, not the liquidity where the sale starts:
/// a close moves the price on and withdraws the leaver's part, and the
/// stayers' part is valued again from where it ended, toward the same limit;
/// on that shorter way, with less liquidity, the threshold is never higher,
/// so no tick crossed exactly before the close is worked in as small after
/// it, and what the leaver's sale fetched and the stayers' part are worth
/// after it add up to at least what everything was worth before.
pub const THETA: u128 = 100;

/// The most initialized ticks of any size Orca's swap in one close may
/// cross, each costing it about 7,500 units: a claim's close at this many,
/// from a position with fees, across a boundary between two of Orca's
/// dynamic tick arrays, measures about 375,000 units, inside
/// `shonin_driver::COMPUTE_PER_DRIVER`. A claim whose own swap would cross
/// more waits; a stopped station's unwind sells up to a unit short of the
/// tick past them, and the next goes on.
pub const SWAP_CROSSINGS: u32 = 7;

/// The initialized ticks between the pool's price and a sale's limit, in
/// the sale's direction, with each one's change of liquidity going up, and
/// the threshold ([`THETA`]) for a sale that way.
#[derive(Clone, Debug, Default)]
pub struct Path {
    pub ticks: Vec<(i32, i128)>,
    pub theta: u128,
}

/// The [`Path`] from the pool's price to the square-root price `limit`
/// through `arrays` (as far as `starts` reach), selling A if `a_to_b`.
pub fn path(
    pool: &Whirlpool,
    arrays: &[TickArray],
    starts: &[i32],
    a_to_b: bool,
    limit: u128,
) -> Result<Path> {
    let spacing = pool.tick_spacing as i32;
    let price = pool.sqrt_price;
    let start = pool.tick_current;
    let mut out = Path {
        ticks: Vec::new(),
        theta: pool.liquidity / THETA,
    };
    if starts.is_empty() || (a_to_b && limit >= price) || (!a_to_b && limit <= price) {
        return Ok(out);
    }
    // How many ticks lie between the price and the limit, bounded both
    // ways: ticks surely on the way, and ticks surely past it; between the
    // two, a tick's own price decides.
    let (high, low) = if price >= limit {
        (price, limit)
    } else {
        (limit, price)
    };
    let gap = high - low;
    let near = gap
        .checked_mul(19_999)
        .map_or(0, |n| n / high)
        .saturating_sub(2);
    let far = gap
        .checked_mul(20_001)
        .map_or(u128::MAX, |n| n / low.max(1))
        .saturating_add(2);
    let mut liquidity = pool.liquidity as i128;
    let mut most = pool.liquidity;
    let mut cursor = start;
    for _ in 0..3 * TICKS_PER_ARRAY + 3 {
        let Some((found, net)) = next_tick(arrays, starts, cursor, spacing, a_to_b)? else {
            break;
        };
        let distance = (start as i64 - found as i64).unsigned_abs() as u128;
        if distance > far {
            break;
        }
        if distance > near {
            let root = math::sqrt_price_at(found).map_err(math_error)?;
            let on_the_way = if a_to_b { root >= limit } else { root <= limit };
            if !on_the_way {
                break;
            }
        }
        out.ticks.push((found, net));
        liquidity += if a_to_b { -net } else { net };
        most = most.max(liquidity.max(0) as u128);
        cursor = if a_to_b { found - 1 } else { found };
    }
    out.theta = most / THETA;
    Ok(out)
}

/// Sells `amount` of token A (`a_to_b`) or of token B in `pool`, no further
/// than the square-root price `limit` nor across more than `crossings` ticks
/// of `path` at least its threshold, the others worked in as [`THETA`]
/// says, through the pool's liquidity less `own`; a sale stops where the
/// arrays at `starts` end.
#[allow(clippy::too_many_arguments)]
pub fn sell(
    pool: &Whirlpool,
    starts: &[i32],
    path: &Path,
    own: &[Own],
    amount: u64,
    a_to_b: bool,
    limit: u128,
    crossings: u32,
) -> Result<Sale> {
    let spacing = pool.tick_spacing as i32;
    let span = TICKS_PER_ARRAY * spacing;
    let fee_rate = pool.fee_rate as u128;
    let mut price = pool.sqrt_price;
    let mut tick = pool.tick_current;
    let mut remaining = amount;
    let mut proceeds: u64 = 0;
    let unsold = Sale {
        end: price,
        ..Sale::default()
    };
    if starts.is_empty() || (a_to_b && limit >= price) || (!a_to_b && limit <= price) {
        return Ok(unsold);
    }
    let lowest = *starts.iter().min().unwrap();
    let highest = *starts.iter().max().unwrap() + span;
    // The ticks crossed exactly (big enough, or the station's own), as many
    // as may be and the one past them; and every small removal on the way.
    let mut exact: Vec<(i32, i128)> = Vec::new();
    let mut removed: u128 = 0;
    for &(at, net) in &path.ticks {
        let own_tick = own.iter().any(|o| o.tick_lower == at || o.tick_upper == at);
        if own_tick || net.unsigned_abs() >= path.theta {
            if exact.len() <= crossings as usize {
                exact.push((at, net));
            }
        } else {
            let change = if a_to_b { -net } else { net };
            if change < 0 {
                removed = removed.saturating_add(change.unsigned_abs());
            }
        }
    }
    let removed = removed.min(i128::MAX as u128) as i128;
    // The pool's liquidity as the exact ticks change it; the sale works with
    // that less every small removal, never below nothing.
    let mut liquidity = pool.liquidity as i128;
    let mut stop = None;
    for at in 0..=exact.len() {
        if remaining == 0 || price == limit || stop.is_some() {
            break;
        }
        // The next exact tick, or the edge of what the arrays cover.
        let next = exact.get(at).copied();
        let next_tick = match next {
            Some((found, _)) => found,
            None if a_to_b => lowest,
            None => highest,
        };
        let tick_price = math::sqrt_price_at(next_tick.clamp(math::MIN_TICK, math::MAX_TICK))
            .map_err(math_error)?;
        // With every crossing allowed made, a unit short of the next exact
        // tick: the sale stops there, crossing nothing more.
        let last = next.is_some() && at == crossings as usize;
        let edge = match (last, a_to_b) {
            (false, _) => tick_price,
            (true, true) => tick_price + 1,
            (true, false) => tick_price - 1,
        };
        let target = if a_to_b {
            edge.max(limit)
        } else {
            edge.min(limit)
        };
        let working = (liquidity - removed).max(0) as u128;
        let theirs = working.saturating_sub(own_at(own, tick));
        let step = step(remaining, fee_rate, theirs, price, target, a_to_b)?;
        remaining -= step.consumed;
        proceeds = proceeds.saturating_add(step.out);
        price = step.next;
        if last {
            if price == edge {
                stop = Some(edge);
            }
            break;
        }
        let Some((found, net)) = next else {
            // At the edge of the given arrays: nothing further is known.
            break;
        };
        if price != tick_price {
            // Sold out or at the limit before the tick.
            break;
        }
        // Crossing it: the liquidity that starts or ends there.
        if a_to_b {
            liquidity -= net;
            tick = found - 1;
        } else {
            liquidity += net;
            tick = found;
        }
    }
    Ok(Sale {
        sold: amount - remaining,
        proceeds,
        stop,
        end: price,
    })
}

/// The station's own liquidity active at `tick`.
fn own_at(own: &[Own], tick: i32) -> u128 {
    own.iter()
        .filter(|o| o.tick_lower <= tick && tick < o.tick_upper)
        .map(|o| o.liquidity)
        .sum()
}

/// The next initialized tick from `tick` in the sale's direction among
/// `arrays`, at or below it selling A, above it selling B, with its
/// liquidity's change going up.
fn next_tick(
    arrays: &[TickArray],
    starts: &[i32],
    tick: i32,
    spacing: i32,
    a_to_b: bool,
) -> Result<Option<(i32, i128)>> {
    let span = TICKS_PER_ARRAY * spacing;
    // The array holding the tick, and the tick's offset in it.
    let mut start = tick.div_euclid(span) * span;
    let mut offset = (tick - start).div_euclid(spacing);
    loop {
        if !starts.contains(&start) {
            return Ok(None);
        }
        // An array not there yet holds no initialized tick.
        if let Some(array) = arrays.iter().find(|a| a.start == start) {
            if let Some(found) = array.next_initialized(offset, a_to_b) {
                let at = start + found * spacing;
                let net = array.tick(at, spacing as u16)?.liquidity_net;
                return Ok(Some((at, net)));
            }
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
