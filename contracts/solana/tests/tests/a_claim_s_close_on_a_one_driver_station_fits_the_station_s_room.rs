//! A mainnet Solana station has one driver: Orca's is the only one approved
//! there, and a station refuses a driver named twice. So a claim's close
//! through it must fit the station's whole room, the compute a transaction
//! asks for less the vault's share (`MAX_COMPUTE - CALLER_COMPUTE`), with
//! margin, however a stranger shapes the pool.
//!
//! The worst set-up built here: the sale's walk is capped at
//! `sale::SALE_SPACINGS` tick spacings whatever the band, so the station's
//! band is patched past the factory's cap until the cap binds (at spacing 64
//! it is about 18.5%); every tick on the way is initialized; a stranger's
//! position ends at every step of the valuation's grid; and ticks at twice
//! the pool's threshold sit right below the price, eight of them or one at
//! every spacing of the walk, so that each sale, and the stayers' valuation
//! after the close, makes every exact crossing it can. The station is
//! closed small and again with more SOL to sell.
//!
//! What the code does here: the worst close measures about 431,000 units
//! (dynamic arrays, exact ticks at every spacing, the larger station), the
//! worst valuation about 298,000. The close is over `COMPUTE_PER_DRIVER`,
//! which is the share each of `MAX_DRIVERS` drivers may take of the room;
//! with one driver the close has the whole room (1,200,000). A valuation
//! stays inside `COMPUTE_PER_DRIVER`.

use anchor_lang::prelude::*;
use anchor_spl::token::spl_token;
use shonin_driver::COMPUTE_PER_DRIVER;
use shonin_solana_tests::orca::*;
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use shonin_station::state::{CALLER_COMPUTE, MAX_COMPUTE};
use solana_signer::Signer;

const LOWER: i32 = -20_480;
const UPPER: i32 = -17_408;

/// The share of the room a close may take: at most half, the rest to spare.
const MARGIN_DIVISOR: u64 = 2;

fn trade_back_and_forth(env: &mut Env, rounds: usize) {
    for _ in 0..rounds {
        env.bob_swaps(20 * SOL, true);
        env.bob_swaps(3_000_000_000, false);
    }
}

fn send_wsol(env: &mut Env, home: &Home, lamports: u64) {
    let bob = env.bob.insecure_clone();
    let ix = spl_token::instruction::transfer(
        &spl_token::ID,
        &env.wsol_of(&bob.pubkey()),
        &env.wsol_of(&home.authority(0)),
        &bob.pubkey(),
        &[],
        lamports,
    )
    .unwrap();
    expect_ok(env.send(vec![ix], &[&bob]));
}

fn patch_band(env: &mut Env, home: &Home, band: u16) {
    let mut account = env.svm.get_account(&home.station).unwrap();
    let mut station: shonin_station::state::Station = env.station(home);
    station.exit_band_bps = band;
    let mut data = account.data.clone();
    station.try_serialize(&mut &mut data[..]).unwrap();
    account.data = data;
    env.svm.set_account(home.station, account).unwrap();
    assert_eq!(env.station(home).exit_band_bps, band);
}

/// Positions a spacing wide, every other spacing from the one below the
/// price down, of `liquidity` each: `2 * count` ticks, none shared.
fn every_other_spacing(env: &mut Env, count: i32, liquidity: u128) {
    let top = env.pool_state().tick.div_euclid(SPACING) * SPACING;
    let ranges: Vec<(i32, i32)> = (0..count)
        .map(|i| (top - (2 * i + 1) * SPACING, top - 2 * i * SPACING))
        .collect();
    env.provide_ranges(&ranges, liquidity);
}

/// The set-up for one case; returns the worst served close and the worst
/// valuation measured on it.
fn worst_of(dynamic: bool, exact_pairs: i32, more_sol: u64) -> (u64, u64) {
    let theta = shonin_orca_driver::network::POOLS[0].theta;
    let mut worst_close = 0u64;
    let mut worst_value = 0u64;
    let mut env = Env::with_orca();
    if dynamic {
        let base = array_start(env.pool_state().tick);
        env.make_dynamic(&[base, base - SPAN]);
    }
    let home = env.orca_home_with_band("orca", 1_000);
    env.station_provides(&home, LOWER, UPPER, 10 * SOL);
    trade_back_and_forth(&mut env, 2);
    expect_ok(env.station_buys_sol(&home, 8_000_000_000));
    let top = env.pool_state().tick.div_euclid(SPACING) * SPACING;
    let bottom = array_start(env.pool_state().tick) - 2 * SPAN;
    env.crowd_below((top - bottom) / SPACING);
    let grid = shonin_orca_driver::sale::VALUE_GRID * SPACING;
    let mut lower = (top - SPACING).div_euclid(grid) * grid - SPACING;
    let mut ranges = Vec::new();
    while lower > bottom {
        ranges.push((lower, UPPER));
        lower -= grid;
    }
    env.provide_ranges(&ranges, theta / 4);
    every_other_spacing(&mut env, exact_pairs, 2 * theta);
    let bob = env.bob.insecure_clone();
    env.svm.airdrop(&bob.pubkey(), 20_000 * SOL).unwrap();
    env.wrap(&bob, 10_000 * SOL);
    if more_sol > 0 {
        send_wsol(&mut env, &home, more_sol);
    }
    env.price_follows_pool();
    let group = env.orca_group(&home.station);
    let case = format!(
        "dynamic {dynamic}, {} exact ticks, {} SOL more",
        2 * exact_pairs,
        more_sol / SOL
    );
    for band in [1_000u16, 9_900] {
        patch_band(&mut env, &home, band);
        env.price_follows_pool();
        let (_, value) = ok(env.driver_value_metered(&group, band));
        worst_value = worst_value.max(value);
        // The largest fraction served, as the keeper retries smaller.
        for tenths in (1..=9).rev() {
            if let Ok((outcome, _, compute)) = env.orca_unwind(&home, WAD / 10 * tenths) {
                if !outcome.blocked {
                    println!("{case}, band {band}: valuation {value}, close of {tenths}/10 in {compute} units");
                    worst_close = worst_close.max(compute);
                    break;
                }
            }
        }
    }
    // A far larger station, valued at every band up to the full one.
    send_wsol(&mut env, &home, 10_000 * SOL - more_sol);
    for band in [1_000u16, 9_900, BPS as u16] {
        patch_band(&mut env, &home, band);
        env.price_follows_pool();
        let (_, value) = ok(env.driver_value_metered(&group, band));
        println!("{case}, 10,000 SOL more, band {band}: valuation {value} units");
        worst_value = worst_value.max(value);
    }
    (worst_close, worst_value)
}

#[test]
fn a_claim_s_close_with_every_exact_crossing_made_fits_a_one_driver_station_s_room() {
    let room = MAX_COMPUTE - CALLER_COMPUTE;
    let mut worst_close = 0u64;
    let mut worst_value = 0u64;
    let mut served_small = true;
    for dynamic in [false, true] {
        for exact_pairs in [4, 16] {
            for more_sol in [0, 60 * SOL] {
                let (close, value) = worst_of(dynamic, exact_pairs, more_sol);
                // The small station's close is always served; a larger one
                // may be outside the band, and then has nothing to measure.
                served_small &= more_sol > 0 || close > 0;
                worst_close = worst_close.max(close);
                worst_value = worst_value.max(value);
            }
        }
    }
    println!("worst close {worst_close} of the room {room}, worst valuation {worst_value} of {COMPUTE_PER_DRIVER}");
    assert!(served_small, "a small station's close was never served");
    assert!(
        worst_close * MARGIN_DIVISOR <= room,
        "worst close {worst_close} leaves too little of the room {room}"
    );
    assert!(
        worst_value * 10 <= COMPUTE_PER_DRIVER * 9,
        "worst valuation {worst_value} of {COMPUTE_PER_DRIVER}"
    );
}
