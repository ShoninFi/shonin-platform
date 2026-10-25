//! Orca's driver's sale (`sale::sell`) on the real mainnet SOL/USDC pool
//! (the fixture), with liquidity placed against it at its grids' steps, just
//! past where its stretches end, and with the pool's price on a step: it
//! never takes or pays more than Orca's swap; a sale stopped by its
//! crossings never values higher than one that isn't; and a claim's close's
//! sale of everything before is never more than the leaver's part through
//! Orca and the stayers' part valued after. The limits reach as far as the
//! sale's tick arrays do, past what the driver walks, so every stretch the
//! driver may meet is swept.

use anchor_lang::prelude::*;
use shonin_orca_driver::math;
use shonin_orca_driver::network::WHIRLPOOL_PROGRAM;
use shonin_orca_driver::orca::{TickArray, Whirlpool};
use shonin_orca_driver::sale::{
    self, Path, Sale, CLOSE_CROSSINGS, CLOSE_GRID, VALUE_CROSSINGS, VALUE_GRID,
};
use std::collections::BTreeMap;

const THETA: u128 = 15_000_000_000_000;
const SPACING: i32 = 4;

/// A pool's case: its name, the positions added to the real pool (lower
/// tick, upper tick, liquidity), and the tick its price is moved to, if any.
type Scenario = (&'static str, Vec<(i32, i32, u128)>, Option<i32>);

fn account(data: &'static [u8]) -> &'static AccountInfo<'static> {
    let key: &'static Pubkey = Box::leak(Box::new(Pubkey::new_unique()));
    let lamports: &'static mut u64 = Box::leak(Box::new(1));
    let data: &'static mut [u8] = Box::leak(data.to_vec().into_boxed_slice());
    Box::leak(Box::new(AccountInfo::new(
        key,
        false,
        false,
        lamports,
        data,
        &WHIRLPOOL_PROGRAM,
        false,
        0,
    )))
}

struct Real {
    pool: Whirlpool,
    arrays: Vec<TickArray<'static>>,
    starts: [i32; 3],
}

fn real() -> Real {
    let pool = Whirlpool::load(account(include_bytes!(
        "../../programs/shonin-orca-driver/fixtures/mainnet-sol-usdc/pool.bin"
    )))
    .unwrap();
    let span = 88 * SPACING;
    let current = pool.tick_current.div_euclid(span) * span;
    let starts = [current, current - span, current - 2 * span];
    let bytes: [&'static [u8]; 5] = [
        include_bytes!(
            "../../programs/shonin-orca-driver/fixtures/mainnet-sol-usdc/tick-array-22880.bin"
        ),
        include_bytes!(
            "../../programs/shonin-orca-driver/fixtures/mainnet-sol-usdc/tick-array-23232.bin"
        ),
        include_bytes!(
            "../../programs/shonin-orca-driver/fixtures/mainnet-sol-usdc/tick-array-23584.bin"
        ),
        include_bytes!(
            "../../programs/shonin-orca-driver/fixtures/mainnet-sol-usdc/tick-array-23936.bin"
        ),
        include_bytes!(
            "../../programs/shonin-orca-driver/fixtures/mainnet-sol-usdc/tick-array-24288.bin"
        ),
    ];
    let arrays: Vec<TickArray<'static>> = bytes
        .into_iter()
        .map(|b| TickArray::load(account(b)).unwrap().unwrap())
        .filter(|a| starts.contains(&a.start))
        .collect();
    assert_eq!(arrays.len(), 3);
    Real {
        pool,
        arrays,
        starts,
    }
}

/// The sale's limit for `band_bps`, as `sale_limit` sets it with Pyth level
/// with the pool: the band's floor or the arrays' reach, whichever is nearer.
fn limit(pool: &Whirlpool, band_bps: u64) -> u128 {
    let pyth = pool.sqrt_price;
    let floor = (pyth * (20_000 - band_bps as u128) / 20_000) + 1;
    let ratio = math::sqrt_price_at(-(2 * 88 * SPACING - SPACING)).unwrap();
    let reach = (((pyth * ratio) >> 64) * 20_025).div_ceil(20_000) + 2;
    floor.max(reach)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum By {
    Value(bool),
    Close(bool),
    Orca,
}

fn worked(pool: &Whirlpool, starts: &[i32], path: &Path, amount: u64, limit: u128, by: By) -> Sale {
    let all = u32::MAX / 2;
    let (theta, crossings, grid, tail) = match by {
        By::Value(l) => (
            THETA,
            if l { VALUE_CROSSINGS } else { all },
            VALUE_GRID,
            true,
        ),
        By::Close(l) => (
            THETA,
            if l { CLOSE_CROSSINGS } else { all },
            CLOSE_GRID,
            true,
        ),
        By::Orca => (0, all, 1, false),
    };
    sale::sell(
        pool,
        starts,
        path,
        theta,
        &[],
        amount,
        true,
        limit,
        crossings,
        tail,
        grid,
    )
    .unwrap()
}

/// The pool after Orca's swap ended at `end`, and the ticks still ahead.
fn after(pool: &Whirlpool, path: &Path, end: u128) -> (Whirlpool, Path) {
    let mut tick = pool.tick_current;
    while math::sqrt_price_at(tick).unwrap() > end {
        tick -= 1;
    }
    // Orca's current tick is the one whose price is at or below `end`.
    let mut pool = pool.clone();
    let crossed: i128 = path.ticks.iter().filter(|t| t.0 > tick).map(|t| t.1).sum();
    pool.liquidity = (pool.liquidity as i128 - crossed) as u128;
    pool.sqrt_price = end;
    pool.tick_current = tick;
    let ticks = path.ticks.iter().copied().filter(|t| t.0 <= tick).collect();
    (pool, Path { ticks })
}

/// A scenario: the real pool with extra positions. A position (lower, upper,
/// L) straddling the price adds to the pool's liquidity; its ticks join the path.
fn scenario(
    real: &Real,
    limit: u128,
    positions: &[(i32, i32, u128)],
    price_on: Option<i32>,
) -> (Whirlpool, Path) {
    let mut pool = real.pool.clone();
    if let Some(edge) = price_on {
        pool.sqrt_price = math::sqrt_price_at(edge).unwrap();
        pool.tick_current = edge;
        // Ticks between the old and new price: liquidity as the real path changes it.
        let base = sale::path(
            &real.pool,
            &real.arrays,
            &real.starts,
            true,
            sale::Reach::Limit(limit),
        )
        .unwrap();
        let crossed: i128 = base.ticks.iter().filter(|t| t.0 > edge).map(|t| t.1).sum();
        pool.liquidity = (pool.liquidity as i128 - crossed) as u128;
    }
    let base = sale::path(
        &pool,
        &real.arrays,
        &real.starts,
        true,
        sale::Reach::Limit(limit),
    )
    .unwrap();
    let mut map: BTreeMap<i32, i128> = base.ticks.iter().copied().collect();
    for &(lower, upper, l) in positions {
        assert!(lower < upper);
        if lower <= pool.tick_current && pool.tick_current < upper {
            pool.liquidity += l;
        }
        *map.entry(lower).or_insert(0) += l as i128;
        *map.entry(upper).or_insert(0) -= l as i128;
    }
    let lowest = *real.starts.iter().min().unwrap();
    let limit_tick = {
        let mut t = pool.tick_current;
        while math::sqrt_price_at(t).unwrap() > limit {
            t -= 1;
        }
        t
    };
    let ticks: Vec<(i32, i128)> = map
        .into_iter()
        .rev()
        .filter(|&(t, net)| t <= pool.tick_current && t >= lowest && t >= limit_tick && net != 0)
        .collect();
    (pool, Path { ticks })
}

fn check(name: &str, pool: &Whirlpool, starts: &[i32], path: &Path, limit: u128) {
    let most = |by: By| {
        let (mut lo, mut hi) = (0u64, 1u64 << 50);
        while hi - lo > 1_000_000 {
            let mid = lo + (hi - lo) / 2;
            if worked(pool, starts, path, mid, limit, by).sold == mid {
                lo = mid
            } else {
                hi = mid
            }
        }
        lo
    };
    let orca = most(By::Orca);
    let whole = most(By::Close(true)).min(most(By::Value(true)));
    for by in [
        By::Value(true),
        By::Value(false),
        By::Close(true),
        By::Close(false),
    ] {
        assert!(most(by) <= orca, "{name} {by:?}: sells more than Orca");
        for k in 1..=40u64 {
            let amount = orca / 40 * k;
            let ours = worked(pool, starts, path, amount, limit, by);
            let theirs = worked(pool, starts, path, amount, limit, By::Orca);
            assert!(
                ours.proceeds <= theirs.proceeds && ours.sold <= theirs.sold,
                "{name} {by:?} {amount}: {ours:?} > {theirs:?}"
            );
            // A crossings-limited sale never values higher than a full one.
            let full = match by {
                By::Value(_) => By::Value(false),
                By::Close(_) => By::Close(false),
                By::Orca => By::Orca,
            };
            let f = worked(pool, starts, path, amount, limit, full);
            assert!(
                ours.proceeds <= f.proceeds,
                "{name} {by:?} {amount}: limited {} > full {}",
                ours.proceeds,
                f.proceeds
            );
        }
    }
    // Adds up: the close's sale of everything before; the leaver's part through Orca; the stayers' after.
    for k in 1..=10u64 {
        let total = whole / 10 * k;
        if total == 0 {
            continue;
        }
        let before = worked(pool, starts, path, total, limit, By::Close(true));
        assert_eq!(before.sold, total, "{name}");
        for parts in 1..=19u64 {
            let part = total / 20 * parts;
            let leaver = worked(pool, starts, path, part, limit, By::Orca);
            assert_eq!(leaver.sold, part);
            let (p2, path2) = after(pool, path, leaver.end);
            let stayers = worked(&p2, starts, &path2, total - part, limit, By::Value(true));
            assert_eq!(
                stayers.sold,
                total - part,
                "{name} {k} {parts}: stayers' part not taken after the close"
            );
            assert!(
                leaver.proceeds + stayers.proceeds + 2 >= before.proceeds,
                "{name} {k} {parts}: leaver {} + stayers {} < before {} (short {})",
                leaver.proceeds,
                stayers.proceeds,
                before.proceeds,
                before.proceeds - leaver.proceeds - stayers.proceeds
            );
        }
    }
}

#[test]
fn on_the_real_pool_a_sale_never_beats_orca_and_a_close_adds_up_whatever_liquidity_is_placed_at_its_steps(
) {
    let real = real();
    let tick = real.pool.tick_current;
    println!(
        "pool tick {tick} liquidity {} price {}",
        real.pool.liquidity, real.pool.sqrt_price
    );
    let close_grid = CLOSE_GRID * SPACING;
    let value_grid = VALUE_GRID * SPACING;
    // Grid steps below the price.
    let first_close = tick.div_euclid(close_grid) * close_grid;
    let first_value = tick.div_euclid(value_grid) * value_grid;
    let small = THETA / 2;
    let big = 2 * THETA;
    for &band in &[100u64, 500, 1_000] {
        let limit = limit(&real.pool, band);
        let scenarios: Vec<Scenario> = vec![
            ("real", vec![], None),
            // Small removals just past each close-grid step: active positions ending a spacing below the step.
            (
                "small ends just past close steps",
                (0..6)
                    .map(|i| (first_close - i * close_grid - SPACING, tick + 400, small))
                    .collect(),
                None,
            ),
            // Small removals exactly on the steps.
            (
                "small ends on close steps",
                (0..6)
                    .map(|i| (first_close - i * close_grid, tick + 400, small))
                    .collect(),
                None,
            ),
            // Small removals one spacing above each step (the last tick of the stretch before).
            (
                "small ends a spacing above close steps",
                (0..6)
                    .map(|i| (first_close - i * close_grid + SPACING, tick + 400, small))
                    .collect(),
                None,
            ),
            // The same on the valuation's grid.
            (
                "small ends just past value steps",
                (0..20)
                    .map(|i| (first_value - i * value_grid - SPACING, tick + 400, small))
                    .collect(),
                None,
            ),
            (
                "small ends on value steps",
                (0..20)
                    .map(|i| (first_value - i * value_grid, tick + 400, small))
                    .collect(),
                None,
            ),
            // Exact ticks just past stretch ends, and small adds between.
            (
                "exact just past steps with small adds",
                (0..6)
                    .flat_map(|i| {
                        [
                            (first_close - i * close_grid - SPACING, tick + 400, big),
                            (
                                first_close - i * close_grid - 3 * SPACING,
                                first_close - i * close_grid - 2 * SPACING,
                                small,
                            ),
                        ]
                    })
                    .collect(),
                None,
            ),
            // Liquidity added at grid boundaries (positions wholly on the way, starting at a step).
            (
                "adds at boundaries",
                (0..6)
                    .map(|i| {
                        (
                            first_close - (i + 1) * close_grid,
                            first_close - i * close_grid,
                            small,
                        )
                    })
                    .collect(),
                None,
            ),
            // A ladder of small positions each spanning a step.
            (
                "ladder across steps",
                (0..20)
                    .map(|i| {
                        (
                            first_value - i * value_grid - 2 * SPACING,
                            first_value - i * value_grid + 2 * SPACING,
                            small,
                        )
                    })
                    .collect(),
                None,
            ),
            // Alternating adds and removes around each step.
            (
                "alternating around steps",
                (0..6)
                    .flat_map(|i| {
                        [
                            (first_close - i * close_grid + SPACING, tick + 400, small),
                            (
                                first_close - i * close_grid - SPACING,
                                first_close - i * close_grid,
                                small / 3,
                            ),
                            (
                                first_close - i * close_grid - 2 * SPACING,
                                tick + 400,
                                small / 2,
                            ),
                        ]
                    })
                    .collect(),
                None,
            ),
            // Price on a grid edge, with small removals just past the steps.
            (
                "price on a close-grid edge",
                (0..6)
                    .map(|i| {
                        (
                            first_close - (i + 1) * close_grid - SPACING,
                            tick + 400,
                            small,
                        )
                    })
                    .collect(),
                Some(first_close),
            ),
            (
                "price on a close-grid edge, ends on steps",
                (0..6)
                    .map(|i| (first_close - (i + 1) * close_grid, tick + 400, small))
                    .collect(),
                Some(first_close),
            ),
            (
                "price on a value-grid edge",
                (0..20)
                    .map(|i| {
                        (
                            first_value - (i + 1) * value_grid - SPACING,
                            tick + 400,
                            small,
                        )
                    })
                    .collect(),
                Some(first_value),
            ),
            // Many exact ticks (spend the crossings) then small removals past them.
            (
                "crossings spent then small ends",
                (0..9)
                    .map(|i| (tick - 4 - 8 * i - SPACING, tick - 4 - 8 * i, big))
                    .chain(
                        (0..6).map(|i| (first_close - i * close_grid - SPACING, tick + 400, small)),
                    )
                    .collect(),
                None,
            ),
        ];
        for (name, positions, edge) in scenarios {
            let (pool, path) = scenario(&real, limit, &positions, edge);
            let name = format!("band {band} {name}");
            check(&name, &pool, &real.starts, &path, limit);
        }
    }
    // Random adversarial sweeps.
    let mut seed: u64 = 11;
    let mut next = || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        seed >> 33
    };
    for round in 0..60 {
        let band = [100u64, 500, 1_000][round % 3];
        let limit = limit(&real.pool, band);
        let edge = match round % 4 {
            0 => Some(first_close),
            1 => Some(first_value),
            _ => None,
        };
        let base_tick = edge.unwrap_or(tick);
        let mut positions = Vec::new();
        for _ in 0..(3 + next() % 25) {
            let depth = (next() % 700) as i32;
            let on_grid = next() % 3;
            let lower = match on_grid {
                0 => {
                    (base_tick - depth).div_euclid(close_grid) * close_grid
                        - SPACING * (next() % 3) as i32
                }
                1 => {
                    (base_tick - depth).div_euclid(value_grid) * value_grid
                        - SPACING * (next() % 3) as i32
                }
                _ => (base_tick - depth).div_euclid(SPACING) * SPACING,
            };
            let upper = if next() % 2 == 0 {
                base_tick + 400
            } else {
                lower + SPACING * (1 + next() % 40) as i32
            };
            let l = match next() % 4 {
                0 => big,
                1 => THETA - 1,
                _ => (next() as u128 % THETA) + 1,
            };
            positions.push((lower, upper, l));
        }
        let (pool, path) = scenario(&real, limit, &positions, edge);
        check(
            &format!("random {round} band {band}"),
            &pool,
            &real.starts,
            &path,
            limit,
        );
    }
}
