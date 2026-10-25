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
//! as a sale may make, what is left is valued as sold on from there through
//! the least liquidity on the rest of the way to the sale's limit, if that
//! takes it all.
//!
//! The stayers' part, valued again after a leaver's close, adds up. The
//! close moved the price from `p0` to `p1` and withdrew the leaver's part of
//! the station's liquidity. Valued from `p1` toward the same limit, over the
//! same ticks beyond `p1` with the same threshold, the sale starts from the
//! pool's liquidity at `p1`, which already holds the small changes crossed
//! between (their sum `S01`). Each stretch of it, ending at some `q`, takes
//! off the deepest fall from `p1` to `q`, so at a price `p` in it it works
//! with `E(p) + min(S01, least S over [p1, q))` (`E` and `S` as in
//! [`sell`], counted from `p0`). The valuation before the close ended its
//! stretch holding `p` at `q` or further on (past `p1` the stretches end at
//! the same ticks, or at more of them with a grid dividing the close's, see
//! [`sell`]), and worked there with `E(p) + min(0, least S up to that
//! end)`, no more, `S01` being one of those sums: at every price past `p1`
//! the sale after works with at least the liquidity the valuation before
//! the close did.
//! What the leaver's sale fetched (through Orca, at least as worked out)
//! and the stayers' part after then add up to at least the whole before;
//! what is left past the crossings is sold on through the least liquidity
//! before (the rest of the way past its stop) and through at least as much
//! after, the crossings counted from `p1` reaching further. So the leaver is paid at
//! most `f` of the valuation before, and the stayers keep at least theirs.
//!
//! Rounding is the pool's at every step, and against the seller. The sale
//! goes no further than its limit, inside the band, and no further than
//! the tick arrays it was given reach (an array not there yet holds no
//! ticks): what is left unsold there fetched nothing, and is worth nothing
//! to `exit`.

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
/// crossing each. What a sale would sell past them is valued as [`sell`]
/// says.
pub const CLOSE_CROSSINGS: u32 = 7;

/// The most ticks at least the threshold a valuation's sale may cross:
/// about 4,000 units each on a valuation's 30,000 to 55,000. From a pool
/// near Pyth's price to the sale's limit at the bottom of the widest band a
/// station may be created with (10%) is about 16 spacings of 64 ticks, all
/// crossed exactly; at a spacing of 4 a sale walks at most
/// [`SALE_SPACINGS`] of them, and more could be exact. Past these, what is left is valued as sold on through
/// the least liquidity on the rest of the way, where that takes it all (see
/// [`sell`]), and the valuation is outside otherwise.
pub const VALUE_CROSSINGS: u32 = 40;

/// The most initialized ticks of any size Orca's swap in one close may
/// cross, each costing it about 7,500 units: a claim's close at this many,
/// from a position with fees, across a boundary between two of Orca's
/// dynamic tick arrays, fits `shonin_driver::COMPUTE_PER_DRIVER`. A claim
/// whose own swap would cross more is refused this try (the vault may serve
/// it in parts); a stopped station's unwind sells up to a unit short of the
/// tick past them, and the next goes on.
pub const SWAP_CROSSINGS: u32 = 7;

/// How many tick spacings apart a valuation's sale may end a stretch to
/// take off the small ticks' deepest fall as it grows (see [`sell`]): over
/// a sale's three tick arrays at most 17 stretches more, each a step of
/// about 3,500 units.
pub const VALUE_GRID: i32 = 16;

/// The same for a close's sale and the sale of everything a claim's close
/// works out first: coarser, as a close has less compute to spare, and a
/// multiple of [`VALUE_GRID`], so the stayers' valuation after a close
/// ends a stretch wherever the close's did.
pub const CLOSE_GRID: i32 = 4 * VALUE_GRID;

/// The most tick spacings past Pyth's price (and the pool's way from it)
/// a sale inside the band is worked out over, whatever the band: a sale
/// meets every initialized tick on its way, at most one a spacing, so this
/// bounds what a valuation and a claim's close cost on any pool. With every
/// tick initialized, a claim's close walking this far, valued again after,
/// fits `shonin_driver::COMPUTE_PER_DRIVER` with room. A band reaching
/// further is sold through this far, as one reaching past the sale's tick
/// arrays is (see the driver's `sale_limit`).
pub const SALE_SPACINGS: i32 = 32;

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
    // One walk through the arrays in the sale's direction, from the pool's
    // tick (itself included selling A, excluded selling B).
    out.ticks
        .reserve(most.min(3 * TICKS_PER_ARRAY as u32) as usize);
    let span = TICKS_PER_ARRAY * spacing;
    let mut start = pool.tick_current.div_euclid(span) * span;
    let mut offset = (pool.tick_current - start).div_euclid(spacing);
    while starts.contains(&start) {
        // An array not there yet holds no initialized tick.
        if let Some(array) = arrays.iter().find(|a| a.start == start) {
            let ticks = &mut out.ticks;
            let more = array.each_initialized(offset, a_to_b, |at, net| {
                let found = start + at * spacing;
                let past = match last {
                    Some(last) => (a_to_b && found < last) || (!a_to_b && found > last),
                    None => false,
                };
                if past || ticks.len() as u32 >= most {
                    return false;
                }
                ticks.push((found, net));
                true
            })?;
            if !more {
                break;
            }
        }
        if a_to_b {
            start -= span;
            offset = TICKS_PER_ARRAY - 1;
        } else {
            start += span;
            offset = -1;
        }
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
/// its deepest fall below nothing so far (the most that small removals have
/// outrun small additions over any stretch from the start) is taken off
/// the liquidity. The sale is worked out in stretches, each ending at an
/// exact tick or at a tick that is a multiple of `grid` spacings (one of
/// those only where the deepest fall grows on the way to the next), and
/// each stretch takes off the deepest fall as it stands at the stretch's
/// end: as the fall grows along the way, the sale works with what the pool
/// keeps there rather than what it keeps at the sale's limit. A small
/// position wholly on the way adds before it removes and costs the sale
/// nothing; a ladder of them costs nothing.
///
/// The sale never fetches more than Orca's swap. Write `E(p)` for the pool's
/// liquidity at a price `p` on the way as the exact ticks alone would change
/// it, and `S(p)` for the sum of the small ticks' changes crossed before
/// `p`; Orca's liquidity there is `E(p) + S(p)`. The deepest fall `D(q)` up
/// to a point `q` is the largest `-S` over every point before `q`, at least
/// nothing, and never shrinks along the way; a stretch ending at `q` holds
/// only points before it, so for each of them `S(p) >= -D(q)` and `E(p) -
/// D(q) <= E(p) + S(p)`: the sale's liquidity (that, never below
/// nothing, less the station's own) is never more than Orca's anywhere. The
/// changes are taken going the sale's way, so the same holds selling A (the
/// price falling, a tick's change the negative of its net) and selling B.
/// Less liquidity at every price moves the price further for each unit sold
/// and pays less for it.
///
/// Where the stretches end depends only on the ticks and `grid`, never on
/// where the sale starts, so a sale from further on the same way (the
/// stayers' after a close) meets the same ends past its start, or more of
/// them with a finer `grid` dividing this one.
///
/// With `tail`, what a sale stopped by `crossings` leaves unsold is valued
/// at what a sale of it from the stop through the least liquidity on the
/// rest of the way to the limit fetches (every exact tick's change applied,
/// each stretch's deepest fall and the station's own taken off), when that
/// takes all of it: then Orca's swap, through at least that liquidity
/// everywhere, takes it too and fetches at least as much. Otherwise it is
/// left unsold, and the sale did not take everything.
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
    grid: i32,
) -> Result<Sale> {
    let ends = Ends::new(pool, path, theta, own, a_to_b, grid);
    sell_along(
        pool, starts, &ends, own, amount, a_to_b, limit, crossings, tail,
    )
}

/// [`sell`] along the stretches [`Ends::new`] found for the same pool,
/// path, threshold, positions, direction and grid: a claim's close sells
/// twice along the same ones, and works them out once.
#[allow(clippy::too_many_arguments)]
pub fn sell_along(
    pool: &Whirlpool,
    starts: &[i32],
    ends: &Ends,
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
    let (ends, last_fall) = (&ends.ends[..], ends.last_fall);
    // The pool's liquidity as the exact ticks change it; each stretch works
    // with that less its deepest fall, never below nothing.
    let mut liquidity = pool.liquidity as i128;
    let mut stop = None;
    let mut crossed: u32 = 0;
    let mut at = 0;
    loop {
        if remaining == 0 || price == limit || stop.is_some() {
            break;
        }
        // The stretch's end: the next exact tick or step of the grid, or the
        // edge of what the arrays cover.
        let next = ends.get(at).copied();
        let (next_tick, fall) = match next {
            Some(end) => (end.at, end.fall),
            None if a_to_b => (lowest, last_fall),
            None => (highest, last_fall),
        };
        let tick_price = math::sqrt_price_at(next_tick.clamp(math::MIN_TICK, math::MAX_TICK))
            .map_err(math_error)?;
        // With every crossing allowed made, a unit short of the next exact
        // tick: the sale stops there, crossing nothing more.
        let last = matches!(next, Some(end) if end.exact) && crossed == crossings;
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
        let working = liquidity.saturating_sub(fall).max(0) as u128;
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
        let Some(end) = next else {
            // At the edge of the given arrays: nothing further is known.
            break;
        };
        if price != tick_price {
            // Sold out or at the limit before the stretch's end.
            break;
        }
        // Past it: an exact tick's liquidity starts or ends there.
        if end.exact {
            liquidity += if a_to_b { -end.net } else { end.net };
            crossed += 1;
        }
        tick = if a_to_b { end.at - 1 } else { end.at };
        at += 1;
    }
    let mut sold = amount - remaining;
    if tail && stop.is_some() && remaining > 0 {
        // Sold on from where the sale stopped through the least liquidity
        // it meets from there to the limit, never more than Orca's at any
        // price on the way: valued at what that fetches if it sells all of
        // it.
        let working = |liquidity: i128, fall: i128, tick: i32| {
            (liquidity.saturating_sub(fall).max(0) as u128).saturating_sub(own_at(own, tick))
        };
        let fall_of = |i: usize| ends.get(i).map_or(last_fall, |end| end.fall);
        let mut least = working(liquidity, fall_of(at), tick);
        for (i, &end) in ends.iter().enumerate().skip(at) {
            if end.exact {
                liquidity += if a_to_b { -end.net } else { end.net };
            }
            tick = if a_to_b { end.at - 1 } else { end.at };
            least = least.min(working(liquidity, fall_of(i + 1), tick));
        }
        let rest = step(remaining, fee_rate, least, price, limit, a_to_b)?;
        if rest.consumed >= remaining {
            proceeds = proceeds.saturating_add(rest.out);
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

/// Where a stretch of a sale ends: an exact tick, with its change of
/// liquidity going up, or a step of the grid; and the deepest fall of the
/// small ticks' running sum before it, which the stretch takes off.
#[derive(Clone, Copy, Debug)]
struct End {
    at: i32,
    net: i128,
    exact: bool,
    fall: i128,
}

/// The ends of a sale's stretches along a [`Path`], in the sale's
/// direction, and each one's deepest fall.
#[derive(Clone, Debug, Default)]
pub struct Ends {
    ends: Vec<End>,
    /// The deepest fall over the whole way, for the stretch past the last
    /// end.
    last_fall: i128,
}

impl Ends {
    /// The ends along `path` from `pool`'s tick, for a sale with `theta`
    /// and `own` as [`sell`] takes them and a grid of `grid` spacings. A
    /// tick at the grid's step belongs to the stretch past it, as Orca
    /// crosses a tick on reaching its price; a step of the grid ends a
    /// stretch only where the deepest fall grows past it before the next
    /// end, other steps changing nothing.
    pub fn new(
        pool: &Whirlpool,
        path: &Path,
        theta: u128,
        own: &[Own],
        a_to_b: bool,
        grid: i32,
    ) -> Self {
        let from = pool.tick_current;
        let grid = grid.max(1) * pool.tick_spacing as i32;
        let mut ends: Vec<End> = Vec::new();
        let (mut running, mut deepest): (i128, i128) = (0, 0);
        // Where the last stretch ended (or the sale starts), for the next
        // step of the grid to lie past.
        let mut after = from;
        let mut first = true;
        for &(at, net) in &path.ticks {
            if net.unsigned_abs() >= theta
                || own.iter().any(|o| o.tick_lower == at || o.tick_upper == at)
            {
                ends.push(End {
                    at,
                    net,
                    exact: true,
                    fall: deepest.saturating_neg(),
                });
                after = at;
                first = false;
                continue;
            }
            running = running.saturating_add(if a_to_b { -net } else { net });
            if running < deepest {
                // The step of the grid this tick lies past (or on), if the
                // sale reaches it after the last end: the stretch to it
                // keeps the fall as it stood.
                let step = if a_to_b {
                    -((-at).div_euclid(grid)) * grid
                } else {
                    at.div_euclid(grid) * grid
                };
                let past = match (first, a_to_b) {
                    (false, true) => step < after,
                    (false, false) => step > after,
                    (true, true) => step <= after,
                    (true, false) => step > after,
                };
                if past {
                    ends.push(End {
                        at: step,
                        net: 0,
                        exact: false,
                        fall: deepest.saturating_neg(),
                    });
                    after = step;
                    first = false;
                }
                deepest = running;
            }
        }
        Ends {
            ends,
            last_fall: deepest.saturating_neg(),
        }
    }
}

/// The station's own liquidity active at `tick`.
fn own_at(own: &[Own], tick: i32) -> u128 {
    own.iter()
        .filter(|o| o.tick_lower <= tick && tick < o.tick_upper)
        .map(|o| o.liquidity)
        .sum()
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
            VALUE_GRID,
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
    fn sol_left_past_a_valuation_s_crossings_is_valued_through_the_rest_where_it_holds_the_depth() {
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

    /// A pool at a spacing of 4 whose liquidity mostly ends on the way down
    /// through ticks under the threshold, with some small additions and a
    /// few exact ticks among them.
    fn thinning(exact_every: i32) -> (Whirlpool, Path) {
        let mut seed: u64 = 7;
        let mut ticks = Vec::new();
        let mut ends: u128 = 0;
        for k in 1..=240 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let small = ((seed >> 33) as u128 % THETA) as i128;
            let net = if k % exact_every == 0 {
                3 * THETA as i128
            } else if seed.is_multiple_of(5) {
                -small / 2
            } else {
                small
            };
            ends += net.max(0) as u128;
            ticks.push((TOP - 4 * k, net));
        }
        let (mut pool, _) = pool_and_path(0, 0, 0);
        pool.liquidity = 50 * THETA + ends;
        (pool, Path { ticks })
    }

    fn limit(pool: &Whirlpool) -> u128 {
        pool.sqrt_price / 20_000 * 19_000 + 1
    }

    /// Who works a sale out: a valuation, a close (each with or without
    /// its limit on crossings), or Orca's swap itself.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum By {
        Value(bool),
        Close(bool),
        Orca,
    }

    /// Selling `amount` of A from `pool` as `by` works it out.
    fn worked(pool: &Whirlpool, path: &Path, amount: u64, limit: u128, by: By) -> Sale {
        let all = u32::MAX / 2;
        let (theta, crossings, grid) = match by {
            By::Value(limited) => (
                THETA,
                if limited { VALUE_CROSSINGS } else { all },
                VALUE_GRID,
            ),
            By::Close(limited) => (
                THETA,
                if limited { CLOSE_CROSSINGS } else { all },
                CLOSE_GRID,
            ),
            By::Orca => (0, all, 1),
        };
        sell(
            pool,
            &[0, -352, -704],
            path,
            theta,
            &[],
            amount,
            true,
            limit,
            crossings,
            by != By::Orca,
            grid,
        )
        .unwrap()
    }

    /// The pool after Orca's swap ended at `end`, and the ticks still ahead.
    fn after(pool: &Whirlpool, path: &Path, end: u128) -> (Whirlpool, Path) {
        let mut tick = TOP;
        while math::sqrt_price_at(tick).unwrap() >= end {
            tick -= 1;
        }
        let mut pool = pool.clone();
        let crossed: i128 = path.ticks.iter().filter(|t| t.0 > tick).map(|t| t.1).sum();
        pool.liquidity = (pool.liquidity as i128 - crossed) as u128;
        pool.sqrt_price = end;
        pool.tick_current = tick;
        let ticks = path.ticks.iter().copied().filter(|t| t.0 <= tick).collect();
        (pool, Path { ticks })
    }

    #[test]
    fn a_sale_through_a_thinning_pool_takes_nearly_what_orca_s_swap_takes_and_never_more() {
        let (pool, path) = thinning(50);
        let limit = limit(&pool);
        let most = |by: By| {
            let (mut lo, mut hi) = (0u64, 1u64 << 50);
            while hi - lo > 1_000 {
                let mid = lo + (hi - lo) / 2;
                if worked(&pool, &path, mid, limit, by).sold == mid {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo
        };
        let orca = most(By::Orca);
        for by in [By::Value(false), By::Close(false)] {
            let ours = most(by);
            assert!(
                ours <= orca && ours >= orca / 100 * 90,
                "{by:?}: {ours} of {orca}"
            );
            for tenths in 1..=10 {
                let amount = orca / 10 * tenths;
                let (a, b) = (
                    worked(&pool, &path, amount, limit, by),
                    worked(&pool, &path, amount, limit, By::Orca),
                );
                assert!(
                    a.proceeds <= b.proceeds && a.sold <= b.sold,
                    "{by:?} {amount}"
                );
            }
        }
    }

    /// A close's sale of everything worked out first, a leaver's part sold
    /// through Orca's swap, and the stayers' part valued after from where it
    /// left the pool: the two after add up to at least the whole before,
    /// with the sales' limits on crossings or without.
    #[test]
    fn the_leaver_s_sale_and_the_stayers_valuation_after_add_up_to_the_close_s_sale_before() {
        for (exact_every, limited) in [(50, false), (5, true)] {
            adds_up(exact_every, limited);
        }
    }

    fn adds_up(exact_every: i32, limited: bool) {
        let (pool, path) = thinning(exact_every);
        let limit = limit(&pool);
        let whole: u64 = 2_000_000_000;
        let before = worked(&pool, &path, whole, limit, By::Close(limited));
        assert_eq!(before.sold, whole);
        assert_eq!(before.stop.is_some(), limited);
        for quarters in 1..=3u64 {
            let part = whole / 4 * quarters;
            let leaver = worked(&pool, &path, part, limit, By::Orca);
            assert_eq!(leaver.sold, part);
            let (pool, path) = after(&pool, &path, leaver.end);
            let stayers = worked(&pool, &path, whole - part, limit, By::Value(limited));
            assert_eq!(stayers.sold, whole - part);
            assert!(
                leaver.proceeds + stayers.proceeds + 2 >= before.proceeds,
                "{quarters}: {} + {} < {}",
                leaver.proceeds,
                stayers.proceeds,
                before.proceeds
            );
        }
    }

    /// Narrow positions of the threshold each placed just below the price,
    /// sharing no tick, make a valuation spend its crossings at once: what
    /// it leaves is sold on through the pool's own depth past them, and
    /// the valuation is what it is without them.
    #[test]
    fn positions_stuffed_below_the_price_to_spend_a_valuation_s_crossings_lower_it_by_nothing() {
        let base = 100_000_000_000_000;
        let (pool, _) = pool_and_path(base, 0, 0);
        let stuffed = Path {
            ticks: (0..VALUE_CROSSINGS as i32 / 2 + 1)
                .flat_map(|k| {
                    let upper = TOP - 4 - 8 * k;
                    [(upper, -(THETA as i128)), (upper - 4, THETA as i128)]
                })
                .collect(),
        };
        let amount = 2_000_000_000_000;
        let clean = sale(&pool, &Path::default(), amount);
        let spent = sale(&pool, &stuffed, amount);
        assert!(spent.stop.is_some(), "{spent:?}");
        assert_eq!((clean.sold, spent.sold), (amount, amount));
        assert!(spent.proceeds + 2 >= clean.proceeds, "{spent:?} {clean:?}");
    }
}
