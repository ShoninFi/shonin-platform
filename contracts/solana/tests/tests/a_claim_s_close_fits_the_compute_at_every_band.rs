//! A claim's close and a valuation through Orca's driver cost the same at
//! every band, on a pool with every tick initialized: a sale walks at most
//! `sale::SALE_SPACINGS` tick spacings, whatever the pool's spacing. The
//! local pool is at spacing 64, where the widest band a station may have
//! (10%) is 16 spacings; at mainnet's spacing of 4 a band is many more (1%,
//! 5% and 10% are about 25, 128 and 175, the last at the sale arrays'
//! reach). So the station's band is set here past the factory's cap to walk
//! as many spacings as those (1,600, 5,600 and 9,900 bps), with every tick
//! on the way initialized and a stranger's positions ending a step of the
//! grid further down each.

use anchor_lang::prelude::*;
use anchor_spl::token::spl_token;
use shonin_driver::COMPUTE_PER_DRIVER;
use shonin_solana_tests::orca::*;
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use solana_signer::Signer;

const LOWER: i32 = -20_480;
const UPPER: i32 = -17_408;

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

#[test]
fn a_claim_s_close_and_a_valuation_fit_the_compute_with_room_at_every_band_on_a_pool_ticked_at_every_spacing(
) {
    let mut worst_close = 0u64;
    let mut worst_value = 0u64;
    for dynamic in [false, true] {
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
        let theta = shonin_orca_driver::network::POOLS[0].theta;
        env.provide_ranges(&ranges, theta / 4);
        env.price_follows_pool();
        let group = env.orca_group(&home.station);
        // Spacings on the way, as mainnet's 1%, 5% and 10% bands at spacing 4
        // (the last capped at the arrays' reach).
        for band in [1_000u16, 1_600, 5_600, 9_900] {
            patch_band(&mut env, &home, band);
            env.price_follows_pool();
            let (v, value) = ok(env.driver_value_metered(&group, band));
            println!(
                "dynamic {dynamic} band {band}: valuation {value} units, outside {} exit {}",
                v.outside, v.exit
            );
            worst_value = worst_value.max(value);
            let mut close = 0;
            for tenths in (1..=9).rev() {
                match env.orca_unwind(&home, WAD / 10 * tenths) {
                    Ok((outcome, _, compute)) => {
                        println!("dynamic {dynamic} band {band}: close of {tenths}/10 blocked {} in {compute} units", outcome.blocked);
                        if !outcome.blocked {
                            close = compute;
                            break;
                        }
                    }
                    Err(e) => {
                        println!(
                            "dynamic {dynamic} band {band}: close of {tenths}/10 failed: {e:?}"
                        );
                    }
                }
            }
            assert!(close > 0, "no close served at band {band}");
            worst_close = worst_close.max(close);
        }
        // Holding more SOL than the pool takes on the way: every stretch walked.
        let bob = env.bob.insecure_clone();
        env.svm.airdrop(&bob.pubkey(), 20_000 * SOL).unwrap();
        env.wrap(&bob, 10_000 * SOL);
        send_wsol(&mut env, &home, 10_000 * SOL);
        for band in [1_600u16, 5_600, 9_900, BPS as u16] {
            patch_band(&mut env, &home, band);
            env.price_follows_pool();
            let (v, value) = ok(env.driver_value_metered(&group, band));
            println!("dynamic {dynamic} band {band}, 10,000 SOL more: valuation {value} units, outside {}", v.outside);
            worst_value = worst_value.max(value);
        }
    }
    // Counted as the whole transaction, the vault's stand-in included, with
    // a tenth of the driver's compute to spare.
    println!("worst close {worst_close}, worst valuation {worst_value}, of {COMPUTE_PER_DRIVER}");
    assert!(
        worst_close * 10 <= COMPUTE_PER_DRIVER * 9 && worst_value * 10 <= COMPUTE_PER_DRIVER * 9,
        "worst close {worst_close}, worst valuation {worst_value}"
    );
}
