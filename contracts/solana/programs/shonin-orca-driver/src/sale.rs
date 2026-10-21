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
//! Ticks under the listed pool's threshold are never stepped through, and
//! are paired (see [`sell`]), so the sale works with no more liquidity than
//! the pool has anywhere on its way and fetches no more than Orca's swap
//! would, the stayers keeping any difference. Past as many exact crossings
//! as a sale may make, what is left is valued at the band's limit, if the
//! rest of the way holds the depth to take it.
//!
//! The stayers' part, valued again after a leaver's close, adds up. The
//! close moved the price from `p0` to `p1` and withdrew the leaver's part of
//! the station's liquidity. Valued from `p1` toward the same limit, over the
//! same ticks beyond `p1` with the same threshold, the sale starts from the
//! pool's liquidity at `p1`, which already holds the small changes crossed
//! between (their sum `S01`); its deepest fall is `max(0, -S01 + falls
//! beyond)`, so what it takes off, less `S01`, is at most the deepest fall
//! from `p0`, which counted `-S01` among its own: at every price past `p1`
//! it works with at least the liquidity the valuation before the close did.
//! What the leaver's sale fetched (through Orca, at least as worked out)
//! and the stayers' part after then add up to at least the whole before;
//! units valued at the limit before are valued there or better after, as
//! the crossings counted from `p1` reach further. So the leaver is paid at
//! most `f` of the valuation before, and the stayers keep at least theirs.
//!
//! Rounding is the pool's at every step, and against the seller. The sale
//! goes no further than the band's limit, and no further than the tick
//! arrays it was given reach (an array not there yet holds no ticks): what
//! is left unsold there fetched nothing, and is worth nothing to `exit`.

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

/// The most ticks at least the listed pool's threshold a close's sale may
/// cross exactly: the sale of everything is worked out twice in a claim's
/// close (before, and for the stayers' part after), about 4,000 units a
/// crossing each. What a sale would sell past them is valued at the band's
/// limit.
pub const CLOSE_CROSSINGS: u32 = 7;

/// The most ticks at least the threshold a valuation's sale may cross:
/// about 4,000 units each on a valuation's 30,000 to 55,000. From a pool
/// near Pyth's price to the sale's limit at the bottom of the widest band a
/// station may be created with (10%) is about 16 spacings of 64 ticks, all
/// crossed exactly; at a spacing of 4 it is about 260, more than any
/// valuation affords. Past these, what is left is valued at the band's
/// limit only where the rest of the way is shown to hold the depth to take
/// it (see [`sell`]), and the valuation is outside otherwise.
pub const VALUE_CROSSINGS: u32 = 40;

/// The most initialized ticks of any size Orca's swap in one close may
/// cross, each costing it about 7,500 units: a claim's close at this many,
/// from a position with fees, across a boundary between two of Orca's
/// dynamic tick arrays, fits `shonin_driver::COMPUTE_PER_DRIVER`. A claim
/// whose own swap would cross more is refused this try (the vault may serve
/// it in parts); a stopped station's unwind sells up to a unit short of the
/// tick past them, and the next goes on.
pub const SWAP_CROSSINGS: u32 = 7;

/// The initialized ticks a sale may meet, in its direction from the pool's
/// price, each with its change of liquidity going up: as far as the sale's
/// limit, or only the first so many.
#[derive(Clone, Debug, Default)]
pub struct Path {
    pub ticks: Vec<(i32, i128)>,
}

/// How far a [`Path`] is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Every initialized tick up to the square-root price `limit` (a tick
    /// exactly at it included), within the arrays given.
    Limit(u128),
    /// The first `n`, within the arrays given.
    First(u32),
}

/// The [`Path`] from the pool's price through `arrays` (as far as `starts`
/// reach), selling A if `a_to_b`, as far as `reach` says.
pub fn path(
    pool: &Whirlpool,
    arrays: &[TickArray],
    starts: &[i32],
    a_to_b: bool,
    reach: Reach,
) -> Result<Path> {
    let spacing = pool.tick_spacing as i32;
    let price = pool.sqrt_price;
    let mut out = Path::default();
    if starts.is_empty() {
        return Ok(out);
    }
    // The last tick on the way, found once: no tick's price is worked out
    // on the walk.
    let (last, most) = match reach {
        Reach::Limit(limit) => {
            if (a_to_b && limit >= price) || (!a_to_b && limit <= price) {
                return Ok(out);
            }
            (
                Some(limit_tick(
                    price,
                    pool.tick_current,
                    limit,
                    a_to_b,
                    spacing,
                )?),
                u32::MAX,
            )
        }
        Reach::First(n) => (None, n),
    };
    let mut cursor = pool.tick_current;
    for _ in 0..3 * TICKS_PER_ARRAY + 3 {
        if out.ticks.len() as u32 >= most {
            break;
        }
        let Some((found, net)) = next_tick(arrays, starts, cursor, spacing, a_to_b)? else {
            break;
        };
        if let Some(last) = last {
            if (a_to_b && found < last) || (!a_to_b && found > last) {
                break;
            }
        }
        out.ticks.push((found, net));
        cursor = if a_to_b { found - 1 } else { found };
    }
    Ok(out)
}

/// The furthest initializable tick (a multiple of `spacing`) a sale from the
/// square-root price `price` (at tick `tick`) to `limit` meets: selling A,
/// the lowest whose price is at or above the limit; selling B, the highest
/// at or below it. Searched among the multiples of the spacing between
/// bounds on how many ticks lie between two prices a ratio `r` apart (`2 ln
/// r / ln 1.0001`, between `19_999 (r - 1) / r` and `20_001 (r - 1)`): a
/// handful of prices worked out, once.
fn limit_tick(price: u128, tick: i32, limit: u128, a_to_b: bool, spacing: i32) -> Result<i32> {
    let (high, low) = if price >= limit {
        (price, limit)
    } else {
        (limit, price)
    };
    let gap = high - low;
    let cap = math::MAX_TICK as u128 * 2;
    let near = gap
        .checked_mul(19_999)
        .map_or(0, |n| n / high)
        .saturating_sub(2)
        .min(cap) as i64;
    let far = gap
        .checked_mul(20_001)
        .map_or(u128::MAX, |n| n / low.max(1))
        .saturating_add(2)
        .min(cap) as i64;
    let spacing = spacing as i64;
    // In units of the spacing, clamped to where ticks may be.
    let units = |t: i64, up: bool| {
        let t = t.clamp(math::MIN_TICK as i64, math::MAX_TICK as i64);
        if up {
            -((-t).div_euclid(spacing))
        } else {
            t.div_euclid(spacing)
        }
    };
    let at = |u: i64| {
        let t = (u * spacing).clamp(math::MIN_TICK as i64, math::MAX_TICK as i64) as i32;
        math::sqrt_price_at(t).map_err(math_error)
    };
    let tick = tick as i64;
    if a_to_b {
        // The lowest multiple at or above the limit.
        let (mut lo, mut hi) = (units(tick - far, false), units(tick - near, true));
        if at(lo)? >= limit {
            return Ok((lo * spacing) as i32);
        }
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if at(mid)? >= limit {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        Ok((hi * spacing) as i32)
    } else {
        // The highest multiple at or below the limit.
        let (mut lo, mut hi) = (units(tick + near, false), units(tick + far, true));
        if at(hi)? <= limit {
            return Ok((hi * spacing) as i32);
        }
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if at(mid)? <= limit {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Ok((lo * spacing) as i32)
    }
}

/// Sells `amount` of token A (`a_to_b`) or of token B in `pool`, no further
/// than the square-root price `limit`, through the pool's liquidity less
/// `own`; a sale stops where the arrays at `starts` end.
///
/// Ticks of `path` changing the liquidity by `theta` or more (and the
/// station's own) are crossed exactly, no more than `crossings` of them: the
/// sale stops a unit short of the next. Smaller ones, which anyone can
/// place for a position's rent, are never stepped through, and are paired:
/// walking the sale's way, the running sum of their changes is kept, and
/// only its deepest fall below nothing (the most that small removals ever
/// outrun small additions over a stretch from the start) is taken off the
/// liquidity from the start. A small position wholly on the way adds before
/// it removes and costs the sale nothing; a ladder of them costs nothing.
///
/// The sale never fetches more than Orca's swap. Write `E(p)` for the pool's
/// liquidity at a price `p` on the way as the exact ticks alone would change
/// it, and `S(p)` for the sum of the small ticks' changes crossed before
/// `p`; Orca's liquidity there is `E(p) + S(p)`. The deepest fall `D` is the
/// largest `-S` over every point on the way, at least nothing, so `S(p) >=
/// -D` everywhere and `E(p) - D <= E(p) + S(p)`: the sale's liquidity (that,
/// never below nothing, less the station's own) is never more than Orca's.
/// The changes are taken going the sale's way, so the same holds selling A
/// (the price falling, a tick's change the negative of its net) and selling
/// B. Less liquidity at every price moves the price further for each unit
/// sold and pays less for it.
///
/// With `tail`, what a sale stopped by `crossings` leaves unsold is valued
/// at the limit, the worst price inside the band, less the pool's fee, when
/// a sale of it from the stop through the least liquidity on the rest of the
/// way to the limit (every exact tick's change applied, the deepest fall of
/// the small ones and the station's own taken off) takes all of it: then
/// Orca's swap, through at least that liquidity everywhere, takes it too,
/// at prices no worse than the limit. Otherwise it is left unsold, and the
/// sale did not take everything.
#[allow(clippy::too_many_arguments)]
pub fn sell(
    pool: &Whirlpool,
    starts: &[i32],
    path: &Path,
    theta: u128,
    own: &[Own],
    amount: u64,
    a_to_b: bool,
    limit: u128,
    crossings: u32,
    tail: bool,
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
    // The ticks crossed exactly, as many as may be and the one past them;
    // and the deepest fall of the small ones' running sum.
    let mut every: Vec<(i32, i128)> = Vec::new();
    let (mut running, mut deepest): (i128, i128) = (0, 0);
    for &(at, net) in &path.ticks {
        let own_tick = own.iter().any(|o| o.tick_lower == at || o.tick_upper == at);
        if own_tick || net.unsigned_abs() >= theta {
            every.push((at, net));
        } else {
            let change = if a_to_b { -net } else { net };
            running = running.saturating_add(change);
            deepest = deepest.min(running);
        }
    }
    let removed = deepest.saturating_neg();
    let exact = &every[..every.len().min(crossings as usize + 1)];
    // The pool's liquidity as the exact ticks change it; the sale works with
    // that less the deepest fall, never below nothing.
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
        let working = liquidity.saturating_sub(removed).max(0) as u128;
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
    let mut sold = amount - remaining;
    if tail && stop.is_some() && remaining > 0 {
        // Valued at the limit only if the rest of the way holds the depth to
        // take it: sold from where the sale stopped through the least
        // liquidity it meets from there to the limit, never more than
        // Orca's at any price on the way, it must sell all of it.
        let working = |liquidity: i128, tick: i32| {
            (liquidity.saturating_sub(removed).max(0) as u128).saturating_sub(own_at(own, tick))
        };
        let mut least = working(liquidity, tick);
        for &(found, net) in &every[crossings as usize..] {
            if a_to_b {
                liquidity -= net;
                tick = found - 1;
            } else {
                liquidity += net;
                tick = found;
            }
            least = least.min(working(liquidity, tick));
        }
        let rest = step(remaining, fee_rate, least, price, limit, a_to_b)?;
        if rest.consumed >= remaining {
            proceeds = proceeds.saturating_add(at_limit(remaining, fee_rate, limit, a_to_b));
            sold = amount;
        }
    }
    Ok(Sale {
        sold,
        proceeds,
        stop,
        end: price,
    })
}

/// What `amount` of the token sold fetches at the square-root price
/// `limit`, less the pool's fee, rounded down.
fn at_limit(amount: u64, fee_rate: u128, limit: u128, a_to_b: bool) -> u64 {
    let less_fee = amount as u128 * (FEE_ONE - fee_rate) / FEE_ONE;
    let raw = W::mul(limit, limit);
    let out = if a_to_b {
        raw.mul_shr128(less_fee)
    } else {
        let (q, _) = (U256::from(less_fee) << 128).div_mod(U256(raw.0));
        if q > U256::from(u128::MAX) {
            u128::MAX
        } else {
            q.low_u128()
        }
    };
    out.min(u64::MAX as u128) as u64
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

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::prelude::Pubkey;

    const SPACING: i32 = 4;
    const TOP: i32 = 340;
    const THETA: u128 = 1_000_000_000;

    /// A pool at a spacing of 4, its price at `TOP`, with `base` liquidity
    /// reaching past the band and `count` ticks of `net` each below the
    /// price, a spacing apart, each taking `net` off as the price falls.
    fn pool_and_path(base: u128, count: i32, net: u128) -> (Whirlpool, Path) {
        let pool = Whirlpool {
            config: Pubkey::default(),
            tick_spacing: SPACING as u16,
            fee_tier_index: SPACING as u16,
            fee_rate: 400,
            protocol_fee_rate: 1_300,
            liquidity: base + count as u128 * net,
            sqrt_price: math::sqrt_price_at(TOP).unwrap() + 1,
            tick_current: TOP,
            mint_a: Pubkey::default(),
            vault_a: Pubkey::default(),
            fee_growth_global_a: 0,
            mint_b: Pubkey::default(),
            vault_b: Pubkey::default(),
            fee_growth_global_b: 0,
        };
        let ticks = (1..=count)
            .map(|k| (TOP - 4 - (k - 1) * SPACING, net as i128))
            .collect();
        (pool, Path { ticks })
    }

    /// Selling A at a 10% band: half of it off Pyth's (here the pool's)
    /// square-root price.
    fn sale(pool: &Whirlpool, path: &Path, amount: u64) -> Sale {
        let limit = pool.sqrt_price / 20_000 * 19_000 + 1;
        // The arrays reach past the limit.
        assert!(math::sqrt_price_at(-704).unwrap() < limit);
        sell(
            pool,
            &[0, -352, -704],
            path,
            THETA,
            &[],
            amount,
            true,
            limit,
            VALUE_CROSSINGS,
            true,
        )
        .unwrap()
    }

    #[test]
    fn sol_left_past_a_valuation_s_crossings_with_no_depth_on_the_rest_of_the_band_is_unsold() {
        // At a spacing of 4 a 10% band holds far more exact ticks than a
        // valuation crosses; past them the pool holds nothing.
        let (pool, path) = pool_and_path(0, VALUE_CROSSINGS as i32 + 1, 1_000_000_000_000);
        let amount = 2_000_000_000_000;
        let sold = sale(&pool, &path, amount);
        assert!(sold.stop.is_some(), "{sold:?}");
        assert!(
            sold.sold < amount,
            "valued past where any sale reaches: {sold:?}"
        );
    }

    #[test]
    fn sol_left_past_a_valuation_s_crossings_is_valued_at_the_limit_where_the_rest_holds_the_depth()
    {
        let (pool, path) = pool_and_path(
            100_000_000_000_000,
            VALUE_CROSSINGS as i32 + 1,
            1_000_000_000_000,
        );
        let amount = 2_000_000_000_000;
        let sold = sale(&pool, &path, amount);
        assert!(sold.stop.is_some(), "{sold:?}");
        assert_eq!(sold.sold, amount, "{sold:?}");
        assert!(sold.proceeds > 0);
    }
}
