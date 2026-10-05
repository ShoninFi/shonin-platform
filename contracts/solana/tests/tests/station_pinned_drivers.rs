//! A station keeps the drivers it was created with. A newer implementation
//! the registry approves later reaches only stations created afterwards; no
//! key, the admin's included and whatever it waits out, changes what a
//! running station pinned. Revoking a pinned driver stops trading through it
//! at once and leaves valuing and closing through it working.

use anchor_lang::prelude::Pubkey;
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use solana_signer::Signer;

#[test]
fn a_driver_approved_later_never_reaches_the_station() {
    let (mut env, home) = Env::with_station();
    env.start_trading(&home);
    let before = env.svm.get_account(&home.station).unwrap();
    // A replacement for `local` waits out its timelock, and more.
    expect_ok(env.propose(ApprovalKind::Driver, local(), shonin_test_driver::ID));
    env.skip(10 * TIMELOCK);
    assert_eq!(env.trading_driver(local()), shonin_test_driver::ID);
    assert_eq!(
        env.svm.get_account(&home.station).unwrap(),
        before,
        "the station's record changed"
    );
    assert_eq!(
        env.station(&home).drivers[0].program,
        shonin_local_driver::ID
    );
    // It still values and closes through what it pinned, and nothing else.
    assert_eq!(env.value(&home).mark, FUNDS);
    let mut through_new = home.clone();
    through_new.drivers = vec![shonin_test_driver::ID];
    let ix = env.ix_value(&through_new);
    let payer = env.payer.insecure_clone();
    expect_code(
        env.send(vec![ix], &[&payer]),
        station_error(StationError::WrongDrivers),
    );
    let (outcome, _) = ok(env.unwind_as_vault(&home, WAD / 2));
    assert_eq!(outcome.sent_home, FUNDS / 2);
    // A station created now pins the replacement.
    let fresh = env.new_home(
        "fresh",
        &[Declared {
            name: local(),
            program: shonin_test_driver::ID,
        }],
    );
    assert_eq!(
        env.station(&fresh).drivers[0].program,
        shonin_test_driver::ID
    );
}

/// Everything the admin can do to the registry, through the timelock or at
/// once, leaves a running station's record exactly as it was.
#[test]
fn the_admin_changes_nothing_about_a_running_stations_drivers() {
    let (mut env, home) = Env::with_station();
    env.start_trading(&home);
    let before = env.svm.get_account(&home.station).unwrap();
    let other = env.final_program();
    expect_ok(env.propose(ApprovalKind::Driver, local(), other));
    expect_ok(env.withdraw(ApprovalKind::Driver, local()));
    expect_ok(env.propose(ApprovalKind::Driver, local(), other));
    env.skip(TIMELOCK);
    expect_ok(env.revoke(ApprovalKind::Driver, local(), other));
    expect_ok(env.revoke(ApprovalKind::Driver, local(), shonin_local_driver::ID));
    expect_ok(env.set_timelock(TIMELOCK * 2));
    env.skip(TIMELOCK * 3);
    let after = env.svm.get_account(&home.station).unwrap();
    assert_eq!(
        after, before,
        "the admin reached a running station's record"
    );
    // Revoked, it still values and closes through what it pinned.
    assert_eq!(env.value(&home).mark, FUNDS);
    let (outcome, _) = ok(env.unwind_as_vault(&home, WAD));
    assert_eq!(outcome.sent_home, FUNDS);
}

#[test]
fn a_revoked_pinned_driver_trades_nothing_and_still_values_and_unwinds() {
    let (mut env, home) = Env::with_station();
    env.start_trading(&home);
    expect_ok(env.revoke(ApprovalKind::Driver, local(), shonin_local_driver::ID));
    let ix = env.ix_venue_place(
        &home.authority(0),
        vec![order(perp(), true, UNIT / 10, PRICE)],
    );
    expect_err(
        env.execute(&home, ix),
        station_error(StationError::NotApproved),
    );
    let alice = env.alice.insecure_clone();
    let executor = env.executor.pubkey();
    expect_err(
        env.approve_key_by(&home, &alice, &executor),
        station_error(StationError::NotApproved),
    );
    assert_eq!(env.value(&home).mark, FUNDS);
    let (outcome, _) = ok(env.unwind_as_vault(&home, WAD / 4));
    assert_eq!(outcome.sent_home, FUNDS / 4);
}

/// The same implementation proposed again after its revocation, once its own
/// timelock passes, is approved again, and the stations pinning it trade
/// again: the registry's rule, not the station's.
#[test]
fn a_revoked_driver_approved_again_after_its_timelock_trades_again() {
    let (mut env, home) = Env::with_station();
    env.start_trading(&home);
    expect_ok(env.revoke(ApprovalKind::Driver, local(), shonin_local_driver::ID));
    expect_ok(env.propose(ApprovalKind::Driver, local(), shonin_local_driver::ID));
    let ix = env.ix_venue_place(
        &home.authority(0),
        vec![order(perp(), true, UNIT / 10, PRICE)],
    );
    expect_err(
        env.execute(&home, ix.clone()),
        station_error(StationError::NotApproved),
    );
    env.skip(TIMELOCK);
    expect_ok(env.execute(&home, ix));
}

#[test]
fn a_station_pins_each_of_several_drivers_in_order() {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home(
        "three",
        &[Declared::local_3(), Declared::test(), Declared::local_2()],
    );
    let station = env.station(&home);
    let pinned: Vec<(Pubkey, Pubkey)> = station
        .drivers
        .iter()
        .map(|d| (d.program, d.venue))
        .collect();
    assert_eq!(
        pinned,
        vec![
            (shonin_local_driver_3::ID, shonin_local_venue_3::ID),
            (shonin_test_driver::ID, shonin_local_venue::ID),
            (shonin_local_driver_2::ID, shonin_local_venue_2::ID),
        ]
    );
    assert_eq!(station.drivers[1].name, test_driver());
    let _ = env.executor.pubkey();
}
