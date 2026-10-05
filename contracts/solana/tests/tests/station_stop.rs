//! Stopping, and what a stopped station does. Anyone may stop a station once
//! its vault has halted and the halt's grace has passed; every key goes, and
//! nothing a driver does can keep it from stopping, since on Solana a key is
//! only the station's record. A stopped station moves money only home. Anyone
//! may unwind all of it, never a part: each venue on its own, inside a band
//! that holds for a week after the stop and then doubles each week until any
//! price is accepted, and only while the vault has no priced valuation to
//! settle. A driver that fails fails the whole transaction (a failed call
//! can't be caught on Solana), so whoever unwinds names the drivers to go
//! through, and one broken driver traps nothing else: not another venue's
//! money, not the idle dollars.

use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use shonin_station::state::WIDEN_AFTER;
use shonin_test_driver::mode;
use solana_signer::Signer;

const WEEK: u64 = 7 * DAY;

fn trading() -> (Env, Home) {
    let (mut env, home) = Env::with_station();
    env.start_trading(&home);
    (env, home)
}

/// Half a BTC bought spot with 60k at the venue, and 40k idle at the
/// station.
fn half_a_btc() -> (Env, Home) {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, 60_000_000_000);
    env.place(&home, vec![order(spot(), true, UNIT / 2, PRICE)]);
    (env, home)
}

#[test]
fn a_station_stops_only_after_its_vault_halts_and_the_grace_passes() {
    let (mut env, home) = trading();
    expect_code(env.stop(&home), station_error(StationError::NotHalted));
    let now = env.now();
    env.set_view(&home, now, HALT_GRACE, false);
    expect_code(env.stop(&home), station_error(StationError::NotHalted));
    env.skip(HALT_GRACE - 1);
    expect_code(env.stop(&home), station_error(StationError::NotHalted));
    env.skip(1);
    expect_ok(env.stop(&home));
    let station = env.station(&home);
    assert!(station.stopped);
    assert_eq!(station.stopped_at, env.now());
    assert_eq!(
        station.drivers[0].key,
        anchor_lang::prelude::Pubkey::default()
    );
    expect_code(env.stop(&home), station_error(StationError::AlreadyStopped));
}

#[test]
fn a_station_always_stops_whatever_its_driver_does() {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("broken", &[Declared::test()]);
    env.approve_executor(&home);
    for bad in [mode::FAIL, mode::BURN, mode::GARBAGE] {
        env.set_mode(bad);
    }
    env.halt_and_stop(&home);
    assert!(env.station(&home).stopped);
}

#[test]
fn a_passer_by_unwinds_only_a_stopped_station_and_only_all_of_it() {
    let (mut env, home) = trading();
    let alice = env.alice.insecure_clone();
    expect_err(
        env.unwind_as(&home, &alice, WAD),
        station_error(StationError::NotAllowed),
    );
    env.halt_and_stop(&home);
    expect_err(
        env.unwind_as(&home, &alice, WAD / 2),
        station_error(StationError::NotAllowed),
    );
    let outcome = ok(env.unwind_as(&home, &alice, WAD));
    assert!(!outcome.blocked);
    assert_eq!(outcome.sent_home, FUNDS);
    assert_eq!(env.usd_balance(&home.vault), FUNDS);
    assert_eq!(env.idle(&home), 0);
    assert_eq!(env.usd_balance(&home.station), 0);
}

#[test]
fn a_stopped_station_moves_nothing_but_home() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS / 2));
    env.approve_executor(&home);
    env.halt_and_stop(&home);
    expect_code(
        env.pull(&home, 1),
        station_error(StationError::AlreadyStopped),
    );
    expect_code(
        env.push(&home, 1),
        station_error(StationError::AlreadyStopped),
    );
    let executor = env.executor.insecure_clone();
    expect_err(
        env.approve_key_by(&home, &executor, &executor.pubkey()),
        station_error(StationError::AlreadyStopped),
    );
    // Its counted dollars come home by the unwind.
    let alice = env.alice.insecure_clone();
    ok(env.unwind_stopped(&home, &alice, &[]));
    assert_eq!(env.usd_balance(&home.vault), FUNDS);
}

#[test]
fn a_stopped_station_closes_a_part_only_for_the_vault() {
    let (mut env, home) = trading();
    env.halt_and_stop(&home);
    // The executor's key is gone, and a key's fractional unwind isn't the
    // vault's.
    let executor = env.executor.insecure_clone();
    let ix = env.ix_unwind(&home, &executor.pubkey(), WAD / 4, true);
    expect_code(
        env.send(vec![ix], &[&executor]),
        station_error(StationError::NotAllowed),
    );
    // The vault's own claim still closes its part, inside the exit band.
    let (outcome, received) = ok(env.unwind_as_vault(&home, WAD / 4));
    assert_eq!(outcome.sent_home, FUNDS / 4);
    assert_eq!(received, FUNDS / 4);
}

#[test]
fn a_stopped_stations_full_unwind_waits_for_settlement() {
    let (mut env, home) = trading();
    env.halt_and_stop(&home);
    let halted = env.now() - HALT_GRACE;
    env.set_view(&home, halted, HALT_GRACE, true);
    let alice = env.alice.insecure_clone();
    expect_err(
        env.unwind_as(&home, &alice, WAD),
        station_error(StationError::Unsettled),
    );
    expect_err(
        env.unwind_stopped(&home, &alice, &[0]),
        station_error(StationError::Unsettled),
    );
    assert_eq!(env.position(&home.authority(0), spot()), UNIT as i128);
    // Settled, anyone unwinds it.
    env.set_view(&home, halted, HALT_GRACE, false);
    ok(env.unwind_as(&home, &alice, WAD));
    assert_eq!(env.usd_balance(&home.vault), FUNDS);
}

#[test]
fn only_a_stopped_station_unwinds_by_name() {
    let (mut env, home) = trading();
    let alice = env.alice.insecure_clone();
    expect_err(
        env.unwind_stopped(&home, &alice, &[0]),
        station_error(StationError::NotHalted),
    );
}

#[test]
fn drivers_named_must_be_the_stations_each_once() {
    let (mut env, home) = trading();
    env.halt_and_stop(&home);
    let alice = env.alice.insecure_clone();
    let mut doubled = home.clone();
    doubled.drivers = vec![home.drivers[0], home.drivers[0]];
    let ix = env.ix_unwind_stopped(&doubled, &alice.pubkey(), &[0, 1]);
    // Two names for a station with one driver: the second doesn't exist.
    expect_code(
        env.send(vec![ix], &[&alice]),
        station_error(StationError::WrongDrivers),
    );
    let mut ix = env.ix_unwind_stopped(&home, &alice.pubkey(), &[0]);
    ix.data = anchor_lang::InstructionData::data(&shonin_station::instruction::UnwindStopped {
        drivers: vec![0, 0],
        groups: vec![8, 8],
    });
    expect_code(
        env.send(vec![ix], &[&alice]),
        station_error(StationError::WrongDrivers),
    );
}

#[test]
fn the_idle_dollars_come_home_with_no_driver_named() {
    let (mut env, home) = half_a_btc();
    env.halt_and_stop(&home);
    let alice = env.alice.insecure_clone();
    let outcome = ok(env.unwind_stopped(&home, &alice, &[]));
    assert_eq!(outcome.sent_home, 40_000_000_000);
    assert_eq!(env.position(&home.authority(0), spot()), (UNIT / 2) as i128);
}

/// One driver that fails, burns its compute or answers garbage, beside a
/// working one at another venue: the full unwind through both fails as a
/// whole (nothing on Solana can catch a failed call), and the unwind through
/// the working one alone brings everything else home.
#[test]
fn one_failing_driver_traps_nothing_else_at_a_stopped_station() {
    for bad in [mode::FAIL, mode::BURN, mode::GARBAGE, mode::SILENT] {
        let mut env = Env::new();
        env.set_up_stations();
        // The working driver first, at the second venue; the test driver at
        // the first.
        let home = env.new_home("two", &[Declared::local_2(), Declared::test()]);
        expect_ok(env.pull(&home, FUNDS));
        env.approve_executor(&home);
        env.fund_venue(&home, 60_000_000_000);
        env.place(&home, vec![order(spot(), true, UNIT / 2, PRICE)]);
        env.set_mode(bad);
        env.halt_and_stop(&home);
        let alice = env.alice.insecure_clone();
        assert!(env.unwind_as(&home, &alice, WAD).is_err(), "mode {bad}");
        let outcome = ok(env.unwind_stopped(&home, &alice, &[0]));
        assert!(!outcome.blocked);
        assert_eq!(
            env.position_at(&VENUES[1], &home.authority(1), spot()),
            0,
            "mode {bad}: the working venue was not unwound"
        );
        assert_eq!(
            env.usd_balance(&home.station),
            0,
            "mode {bad}: idle dollars stayed"
        );
        assert_eq!(
            env.usd_balance(&home.vault),
            FUNDS,
            "mode {bad}: the money did not come home"
        );
    }
}

#[test]
fn a_stopped_stations_band_holds_a_week_then_doubles_weekly() {
    let (mut env, home) = trading();
    let band = |env: &Env| env.station(&home).stopped_band(env.now());
    assert_eq!(band(&env), EXIT_BAND, "a running station's band");
    env.halt_and_stop(&home);
    assert_eq!(band(&env), 100);
    env.skip(WEEK - 1);
    assert_eq!(band(&env), 100, "widened inside the first week");
    env.skip(1);
    assert_eq!(band(&env), 200);
    env.skip(WEEK);
    assert_eq!(band(&env), 400);
    env.skip(4 * WEEK);
    assert_eq!(band(&env), 6_400);
    env.skip(WEEK);
    assert_eq!(band(&env), 10_000, "never past any price");
    env.skip(365 * DAY);
    assert_eq!(band(&env), 10_000);
    assert_eq!(WIDEN_AFTER, WEEK);
}

#[test]
fn a_market_outside_the_band_for_good_is_unwound_once_the_band_reaches_it() {
    let (mut env, home) = trading();
    // The spot market's bid sits 10% under its own price, for good.
    env.set_prices(spot(), PRICE, PRICE, PRICE * 90 / 100, PRICE);
    env.halt_and_stop(&home);
    let alice = env.alice.insecure_clone();
    let outcome = ok(env.unwind_as(&home, &alice, WAD));
    assert!(outcome.blocked, "sold through a market outside the band");
    assert!(env.position(&home.authority(0), spot()) > 0);
    // 2%, 4% and 8% still don't reach it; 16% does.
    env.skip(3 * WEEK);
    assert!(
        ok(env.unwind_as(&home, &alice, WAD)).blocked,
        "sold before the band reached it"
    );
    env.skip(WEEK);
    let outcome = ok(env.unwind_as(&home, &alice, WAD));
    assert!(!outcome.blocked, "the market stayed out of reach");
    assert_eq!(env.position(&home.authority(0), spot()), 0);
    assert_eq!(env.position(&home.authority(0), perp()), 0);
    assert_eq!(
        env.usd_balance(&home.station),
        0,
        "money stayed at the station"
    );
    assert!(env.usd_balance(&home.vault) > 90_000_000_000);
}

#[test]
fn a_mark_stuck_above_twice_the_venues_price_is_unwound_at_the_full_band() {
    let (mut env, home) = trading();
    env.set_prices(spot(), PRICE, PRICE / 3, PRICE / 3, PRICE / 3);
    env.set_prices(perp(), PRICE, PRICE / 3, PRICE / 3, PRICE / 3);
    env.halt_and_stop(&home);
    env.skip(20 * WEEK);
    assert_eq!(env.station(&home).stopped_band(env.now()), 10_000);
    let alice = env.alice.insecure_clone();
    let outcome = ok(env.unwind_as(&home, &alice, WAD));
    assert!(
        !outcome.blocked,
        "any price is accepted, yet the close was refused"
    );
    assert_eq!(env.position(&home.authority(0), spot()), 0);
}

#[test]
fn a_shorts_ask_above_twice_the_venues_price_is_unwound_at_the_full_band() {
    let (mut env, home) = trading();
    env.set_prices(perp(), PRICE, PRICE, PRICE, PRICE * 3);
    env.halt_and_stop(&home);
    env.skip(20 * WEEK);
    let alice = env.alice.insecure_clone();
    let outcome = ok(env.unwind_as(&home, &alice, WAD));
    assert!(!outcome.blocked);
    assert_eq!(
        env.position(&home.authority(0), perp()),
        0,
        "the short is still open"
    );
}

/// The band the stop's rule gives for `elapsed` after it.
fn band_after(band: u64, elapsed: u64) -> u64 {
    if elapsed < WEEK {
        return band;
    }
    let mut expected = band.max(1);
    let mut weeks = elapsed / WEEK;
    while weeks > 0 && expected < BPS {
        expected *= 2;
        weeks -= 1;
    }
    expected.min(BPS)
}

#[test]
fn a_stopped_stations_band_follows_only_the_time_since_the_stop() {
    let mut rng = Rng::new(seed());
    let (mut env, home) = half_a_btc();
    let band = |env: &Env| env.station(&home).stopped_band(env.now()) as u64;
    // Running, and halted but not yet stopped: the exit band, however long.
    for _ in 0..runs(32) {
        env.skip(rng.below(30 * DAY));
        assert_eq!(
            band(&env),
            EXIT_BAND as u64,
            "a running station's band widened"
        );
    }
    let now = env.now();
    env.set_view(&home, now, HALT_GRACE, false);
    env.skip(rng.below(60 * DAY));
    assert_eq!(
        band(&env),
        EXIT_BAND as u64,
        "a halted station's band widened before it stopped"
    );
    expect_ok(env.stop(&home));
    let stopped_at = env.now();
    for _ in 0..runs(64) {
        env.skip(rng.below(10 * DAY));
        assert_eq!(
            band(&env),
            band_after(EXIT_BAND as u64, env.now() - stopped_at)
        );
    }
}

/// With the bid held some spread under the venue's price, a stopped station's
/// full unwind is blocked exactly while the spread is wider than the band as
/// it has widened, and sells everything once it isn't.
#[test]
fn the_widened_band_bounds_a_stopped_stations_full_unwind() {
    let mut rng = Rng::new(seed());
    for _ in 0..runs(24) {
        let (mut env, home) = half_a_btc();
        env.halt_and_stop(&home);
        let weeks = rng.below(16);
        env.skip(weeks * WEEK);
        let spread = rng.below(BPS);
        let bid = PRICE * (BPS - spread) / BPS;
        env.set_prices(spot(), PRICE, PRICE, bid, PRICE);
        let expected = band_after(EXIT_BAND as u64, weeks * WEEK);
        let outside = expected < BPS && bid < (PRICE * (BPS - expected)).div_ceil(BPS);
        let before = env.usd_balance(&home.vault);
        let alice = env.alice.insecure_clone();
        let outcome = ok(env.unwind_as(&home, &alice, WAD));
        assert_eq!(
            outcome.blocked, outside,
            "spread {spread} after {weeks} weeks"
        );
        if outside {
            assert_eq!(
                env.position(&home.authority(0), spot()),
                (UNIT / 2) as i128,
                "a blocked unwind sold"
            );
            // The idle dollars came home all the same.
            assert_eq!(outcome.sent_home, 40_000_000_000);
        } else {
            assert_eq!(
                env.position(&home.authority(0), spot()),
                0,
                "an unwind inside the band left a position"
            );
            assert_eq!(
                env.usd_balance(&home.station),
                0,
                "an unwind inside the band left idle dollars"
            );
        }
        assert_eq!(env.usd_balance(&home.vault), before + outcome.sent_home);
    }
}
