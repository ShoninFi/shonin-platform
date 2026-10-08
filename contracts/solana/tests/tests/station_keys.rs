//! Keys: one executor at a time, approved by anyone on the registry's word
//! alone. A key blessed as this vault's executor, for its bundle, on this
//! cluster, is approved; nothing else is. While the key in place is still
//! blessed, only the key being approved can take its place, so nobody can
//! hand the station back and forth during a handover. A passer-by can
//! withdraw a key only once its blessing has lapsed or its driver is revoked.
//! On Solana a key lives only in the station's record, so withdrawing one is
//! a write that can't fail: there is no venue-side approval to undo.

use shonin_registry::error::RegistryError;
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn funded(env: &mut Env) -> Keypair {
    let key = Keypair::new();
    env.svm.airdrop(&key.pubkey(), 1_000_000_000).unwrap();
    key
}

#[test]
fn a_blessed_executor_is_approved_until_its_blessing_runs_out() {
    let (mut env, home) = Env::with_station();
    let executor = env.executor.insecure_clone();
    let alice = env.alice.insecure_clone();
    // Anyone may send it.
    let meta = expect_ok(env.approve_key_by(&home, &alice, &executor.pubkey()));
    let driver = &env.station(&home).drivers[0];
    assert_eq!(driver.key, executor.pubkey());
    // Its blessing is read again whenever it acts; the approval says until
    // when it runs, as the registry says now.
    let approved = events::<shonin_station::events::KeyApproved>(&meta.logs);
    assert_eq!(approved.len(), 1);
    assert_eq!(
        approved[0].valid_until,
        env.blessed_until(
            &executor.pubkey(),
            env.cluster(),
            home.vault.to_bytes(),
            home.bundle
        )
    );
}

#[test]
fn an_unblessed_or_unwinder_or_elsewhere_blessed_key_is_never_approved() {
    let (mut env, home) = Env::with_station();
    let alice = env.alice.insecure_clone();
    expect_err(
        env.approve_key_by(&home, &alice, &alice.pubkey()),
        station_error(StationError::NotBlessed),
    );
    // An unwinder blessed for this very vault.
    let unwinder = funded(&mut env);
    expect_ok(env.propose_image(unwinder_image(), ImageKind::Unwinder));
    env.skip(TIMELOCK);
    let leaf = env.leaf_for(home.vault.to_bytes(), home.bundle, unwinder.pubkey());
    let journal = env.journal_for(unwinder_image(), unwinder.pubkey(), leaf);
    let cluster = env.cluster();
    expect_ok(env.bless(
        &journal,
        cluster,
        home.vault.to_bytes(),
        home.bundle,
        vec![],
    ));
    expect_err(
        env.approve_key_by(&home, &unwinder, &unwinder.pubkey()),
        station_error(StationError::NotBlessed),
    );
    // An executor blessed for another vault, and one for another bundle.
    let elsewhere = funded(&mut env);
    env.bless_executor_key(elsewhere.pubkey(), [7; 32], home.bundle);
    expect_err(
        env.approve_key_by(&home, &elsewhere, &elsewhere.pubkey()),
        station_error(StationError::NotBlessed),
    );
    let other_bundle = funded(&mut env);
    env.bless_executor_key(
        other_bundle.pubkey(),
        home.vault.to_bytes(),
        hash("another bundle"),
    );
    expect_err(
        env.approve_key_by(&home, &other_bundle, &other_bundle.pubkey()),
        station_error(StationError::NotBlessed),
    );
    assert_eq!(env.station(&home).drivers[0].key, Pubkey::default());
}

#[test]
fn one_executor_at_a_time() {
    let (mut env, home) = Env::with_station();
    env.approve_executor(&home);
    let successor = funded(&mut env);
    env.bless_executor_key(successor.pubkey(), home.vault.to_bytes(), home.bundle);
    // The new instance takes over itself...
    expect_ok(env.approve_key_by(&home, &successor, &successor.pubkey()));
    assert_eq!(env.station(&home).drivers[0].key, successor.pubkey());
    // ...and the old one trades nothing more.
    let ix = env.ix_venue_open(&env.executor.pubkey(), &home.authority(0));
    expect_err(
        env.execute(&home, ix),
        station_error(StationError::NotTheKey),
    );
}

/// A station with two drivers, at two venues, and the primary executor
/// approved through both.
fn two_drivers(env: &mut Env) -> (Home, Keypair) {
    env.set_up_stations();
    let home = env.new_home("two", &[Declared::local(), Declared::local_2()]);
    expect_ok(env.pull(&home, FUNDS));
    let primary = env.executor.insecure_clone();
    expect_ok(env.approve_key_at(&home, &primary, 0, &primary.pubkey()));
    expect_ok(env.approve_key_at(&home, &primary, 1, &primary.pubkey()));
    (home, primary)
}

/// One executor at a time across the whole station, not per driver: a
/// standby approving itself through one driver withdraws the primary's key
/// from every driver, so the two never trade the same vault at once.
#[test]
fn one_executor_at_a_time_across_every_driver() {
    let mut env = Env::new();
    let (home, primary) = two_drivers(&mut env);
    env.fund_venue_at(&home, 0, FUNDS / 2);
    let standby = funded(&mut env);
    env.bless_executor_key(standby.pubkey(), home.vault.to_bytes(), home.bundle);
    expect_ok(env.approve_key_at(&home, &standby, 1, &standby.pubkey()));
    let station = env.station(&home);
    assert_eq!(station.drivers[1].key, standby.pubkey());
    assert_eq!(station.drivers[0].key, Pubkey::default());
    // The primary trades through neither driver...
    let deposit = env.ix_venue_deposit(&home.authority(0), &home.authority_dollars(0), 1);
    expect_err(
        env.execute_as_funded(&home, &primary, 0, deposit, 1),
        station_error(StationError::NotTheKey),
    );
    let open = env.ix_venue_open_at(&VENUES[1], &primary.pubkey(), &home.authority(1));
    expect_err(
        env.execute_as(&home, &primary, 1, open),
        station_error(StationError::NotTheKey),
    );
    // ...and the standby trades through the one it approved, and approves
    // itself through the other.
    let open = env.ix_venue_open_at(&VENUES[1], &standby.pubkey(), &home.authority(1));
    expect_ok(env.execute_as(&home, &standby, 1, open));
    expect_ok(env.approve_key_at(&home, &standby, 0, &standby.pubkey()));
    let deposit = env.ix_venue_deposit(&home.authority(0), &home.authority_dollars(0), 1);
    expect_ok(env.execute_as_funded(&home, &standby, 0, deposit, 1));
}

/// Through any driver, approving another key while the one in place is
/// still blessed is the new key's own act: a passer-by can't do it, and can
/// once that key's blessing has lapsed, which withdraws it everywhere.
#[test]
fn a_passer_by_cannot_unseat_a_still_blessed_executor_through_another_driver() {
    let mut env = Env::new();
    let (home, primary) = two_drivers(&mut env);
    let alice = env.alice.insecure_clone();
    let standby = funded(&mut env);
    env.bless_executor_key(standby.pubkey(), home.vault.to_bytes(), home.bundle);
    expect_err(
        env.approve_key_at(&home, &alice, 1, &standby.pubkey()),
        station_error(StationError::NotAllowed),
    );
    assert_eq!(env.station(&home).drivers[1].key, primary.pubkey());
    env.skip(BLESSING_TTL + 1);
    env.bless_executor_key(standby.pubkey(), home.vault.to_bytes(), home.bundle);
    expect_ok(env.approve_key_at(&home, &alice, 1, &standby.pubkey()));
    let station = env.station(&home);
    assert_eq!(station.drivers[1].key, standby.pubkey());
    assert_eq!(station.drivers[0].key, Pubkey::default());
}

#[test]
fn a_passer_by_cannot_unseat_a_still_blessed_executor() {
    let (mut env, home) = Env::with_station();
    env.approve_executor(&home);
    let successor = funded(&mut env);
    env.bless_executor_key(successor.pubkey(), home.vault.to_bytes(), home.bundle);
    let alice = env.alice.insecure_clone();
    expect_err(
        env.approve_key_by(&home, &alice, &successor.pubkey()),
        station_error(StationError::NotAllowed),
    );
    assert_eq!(env.station(&home).drivers[0].key, env.executor.pubkey());
}

#[test]
fn a_passer_by_cannot_put_a_retired_key_back() {
    let (mut env, home) = Env::with_station();
    let successor = funded(&mut env);
    env.bless_executor_key(successor.pubkey(), home.vault.to_bytes(), home.bundle);
    expect_ok(env.approve_key_by(&home, &successor, &successor.pubkey()));
    let alice = env.alice.insecure_clone();
    let old = env.executor.pubkey();
    expect_err(
        env.approve_key_by(&home, &alice, &old),
        station_error(StationError::NotAllowed),
    );
    assert_eq!(env.station(&home).drivers[0].key, successor.pubkey());
}

#[test]
fn a_lapsed_key_is_replaced_by_anyone() {
    let (mut env, home) = Env::with_station();
    env.approve_executor(&home);
    env.skip(BLESSING_TTL + 1);
    let successor = funded(&mut env);
    env.bless_executor_key(successor.pubkey(), home.vault.to_bytes(), home.bundle);
    let alice = env.alice.insecure_clone();
    expect_ok(env.approve_key_by(&home, &alice, &successor.pubkey()));
    assert_eq!(env.station(&home).drivers[0].key, successor.pubkey());
}

#[test]
fn replacing_a_key_by_someone_else_needs_the_key_in_places_records() {
    let (mut env, home) = Env::with_station();
    env.approve_executor(&home);
    let successor = funded(&mut env);
    env.bless_executor_key(successor.pubkey(), home.vault.to_bytes(), home.bundle);
    let alice = env.alice.insecure_clone();
    let mut ix = env.ix_approve_key(&home, &alice.pubkey(), 0, &successor.pubkey());
    // Leave the key in place's records out (the program's address stands for
    // an account not given).
    let n = ix.accounts.len();
    for meta in &mut ix.accounts[n - 4..] {
        meta.pubkey = shonin_station::ID;
    }
    expect_code(
        env.send(vec![ix], &[&alice]),
        station_error(StationError::MissingRecords),
    );
    // Or with another key's records in their place: the registry refuses
    // the substitution.
    let mut ix = env.ix_approve_key(&home, &alice.pubkey(), 0, &successor.pubkey());
    let theirs = env.key_records(&home, &successor.pubkey());
    let n = ix.accounts.len();
    ix.accounts[n - 4].pubkey = theirs[1];
    ix.accounts[n - 3].pubkey = theirs[2];
    expect_code(
        env.send(vec![ix], &[&alice]),
        registry_error(RegistryError::AccountSubstituted),
    );
}

#[test]
fn a_passer_by_can_revoke_only_a_key_that_has_lapsed() {
    let (mut env, home) = Env::with_station();
    env.approve_executor(&home);
    let executor = env.executor.pubkey();
    let alice = env.alice.insecure_clone();
    let ix = env.ix_revoke_key(&home, 0, &executor);
    expect_code(
        env.send(vec![ix], &[&alice]),
        station_error(StationError::StillBlessed),
    );
    env.skip(BLESSING_TTL + 1);
    let ix = env.ix_revoke_key(&home, 0, &executor);
    expect_ok(env.send(vec![ix], &[&alice]));
    assert_eq!(env.station(&home).drivers[0].key, Pubkey::default());
    // Only the key in place can be revoked.
    let ix = env.ix_revoke_key(&home, 0, &executor);
    expect_code(
        env.send(vec![ix], &[&alice]),
        station_error(StationError::NotTheKey),
    );
}

#[test]
fn revoking_a_driver_stops_the_executor_trading_at_its_venue() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS / 2));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS / 2);
    expect_ok(env.revoke(ApprovalKind::Driver, local(), shonin_local_driver::ID));
    // No new key, no new trade...
    let alice = env.alice.insecure_clone();
    let executor = env.executor.pubkey();
    expect_err(
        env.approve_key_by(&home, &alice, &executor),
        station_error(StationError::NotApproved),
    );
    let ix = env.ix_venue_place(
        &home.authority(0),
        vec![order(perp(), true, UNIT / 2, PRICE)],
    );
    expect_err(
        env.execute(&home, ix),
        station_error(StationError::NotApproved),
    );
    // ...and anyone may withdraw the key at once, blessed or not. Trading
    // stays refused to every key...
    let ix = env.ix_revoke_key(&home, 0, &executor);
    expect_ok(env.send(vec![ix], &[&alice]));
    let ix = env.ix_venue_place(
        &home.authority(0),
        vec![order(perp(), true, UNIT / 2, PRICE)],
    );
    expect_err(
        env.execute(&home, ix),
        station_error(StationError::NotTheKey),
    );
    // ...while bringing money back needs only a blessed executor.
    let ix = env.ix_venue_withdraw(&home.authority(0), &home.authority_dollars(0), 1);
    expect_ok(env.execute(&home, ix));
}

/// Once a driver is revoked, no key can be approved for it and anyone may
/// withdraw the one that was, so its exits depend on neither: any key blessed
/// as this vault's executor brings the venue's cash back to the station,
/// counted, approved or not; a key that isn't blessed brings nothing.
#[test]
fn a_revoked_drivers_exits_need_only_a_blessed_executor() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS / 2);
    expect_ok(env.revoke(ApprovalKind::Driver, local(), shonin_local_driver::ID));
    // A passer-by withdraws the still-blessed executor's key, and no key can
    // be approved for the revoked driver again.
    let alice = env.alice.insecure_clone();
    let executor = env.executor.insecure_clone();
    let ix = env.ix_revoke_key(&home, 0, &executor.pubkey());
    expect_ok(env.send(vec![ix], &[&alice]));
    expect_err(
        env.approve_key_by(&home, &executor, &executor.pubkey()),
        station_error(StationError::NotApproved),
    );

    let withdraw = env.ix_venue_withdraw(&home.authority(0), &home.authority_dollars(0), FUNDS / 2);
    let stranger = funded(&mut env);
    expect_err(
        env.execute_as(&home, &stranger, 0, withdraw.clone()),
        station_error(StationError::NotBlessed),
    );
    // Another executor blessed for this vault, never approved anywhere.
    let standby = funded(&mut env);
    env.bless_executor_key(standby.pubkey(), home.vault.to_bytes(), home.bundle);
    let idle = env.idle(&home);
    expect_ok(env.execute_as(&home, &standby, 0, withdraw));
    assert_eq!(env.venue_cash(&home.authority(0)), 0);
    assert_eq!(env.idle(&home), idle + FUNDS / 2);
}

#[test]
fn a_stopped_station_approves_no_key_and_keeps_none() {
    let (mut env, home) = Env::with_station();
    env.approve_executor(&home);
    env.halt_and_stop(&home);
    assert_eq!(env.station(&home).drivers[0].key, Pubkey::default());
    let executor = env.executor.insecure_clone();
    expect_err(
        env.approve_key_by(&home, &executor, &executor.pubkey()),
        station_error(StationError::AlreadyStopped),
    );
}

#[test]
fn a_driver_the_station_never_declared_is_refused() {
    let (mut env, home) = Env::with_station();
    let executor = env.executor.insecure_clone();
    let mut ix = env.ix_approve_key(&home, &executor.pubkey(), 0, &executor.pubkey());
    ix.data = anchor_lang::InstructionData::data(&shonin_station::instruction::ApproveKey {
        driver: name("hyperliquid"),
        key: executor.pubkey(),
    });
    expect_code(
        env.send(vec![ix], &[&executor]),
        station_error(StationError::NotDeclared),
    );
}

use anchor_lang::prelude::Pubkey;
