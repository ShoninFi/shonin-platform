//! What Solana adds: every station instruction reads the station at its
//! vault's address and nowhere else; the station's and the vault's dollar
//! accounts, and the station's authority at a venue and that authority's
//! dollar account, are the ones the station recorded; the right accounts sign and no
//! others; no two stations, and no station and the settings, share an
//! address; whoever creates an account pays its rent, and the station, which
//! is never closed, holds exactly its rent; a driver is never handed the
//! station's signature, whatever it tries; and time is the clock's timestamp,
//! never the slot.

use std::collections::HashSet;

use anchor_lang::prelude::Pubkey;
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use shonin_test_driver::mode;
use solana_account::Account;
use solana_clock::Clock;
use solana_instruction::Instruction;
use solana_signer::Signer;

fn trading() -> (Env, Home) {
    let (mut env, home) = Env::with_station();
    env.start_trading(&home);
    (env, home)
}

/// A copy of the station's account, owned by the station program, at another
/// address.
fn copy_of_station(env: &mut Env, home: &Home) -> Pubkey {
    let copy = env.svm.get_account(&home.station).unwrap();
    let fake = Pubkey::new_unique();
    env.svm
        .set_account(
            fake,
            Account {
                executable: false,
                rent_epoch: 0,
                ..copy
            },
        )
        .unwrap();
    fake
}

fn replace(ix: &mut Instruction, from: Pubkey, to: Pubkey) {
    for meta in &mut ix.accounts {
        if meta.pubkey == from {
            meta.pubkey = to;
        }
    }
}

#[test]
fn every_instruction_reads_the_station_at_its_vaults_address_only() {
    let (mut env, home) = trading();
    let fake = copy_of_station(&mut env, &home);
    let seeds = anchor_error(anchor_lang::error::ErrorCode::ConstraintSeeds);
    let executor = env.executor.insecure_clone();
    let payer = env.payer.insecure_clone();

    let mut ix = env.ix_execute(
        &home,
        &executor.pubkey(),
        0,
        env.ix_venue_place(&home.authority(0), vec![]),
    );
    ix.accounts[1].pubkey = fake;
    expect_code(env.send(vec![ix], &[&executor]), seeds);

    let mut ix = env.ix_revoke_key(&home, 0, &executor.pubkey());
    ix.accounts[0].pubkey = fake;
    expect_code(env.send(vec![ix], &[&payer]), seeds);

    let mut ix = env.ix_value(&home);
    ix.accounts[0].pubkey = fake;
    expect_code(env.send(vec![ix], &[&payer]), seeds);

    let mut ix = env.ix_stop(&home);
    ix.accounts[0].pubkey = fake;
    expect_code(env.send(vec![ix], &[&payer]), seeds);

    for mut ix in [
        env.ix_pull(&home, 1),
        env.ix_push(&home, 1),
        env.ix_unwind(&home, &home.vault, WAD / 2, false),
    ] {
        replace(&mut ix, home.station, fake);
        expect_code(env.as_vault(&home, ix), seeds);
    }
}

#[test]
fn the_dollar_accounts_are_the_ones_the_station_recorded() {
    let (mut env, home) = trading();
    let other = env.new_home("other", &[Declared::local()]);
    let address = anchor_error(anchor_lang::error::ErrorCode::ConstraintAddress);
    // The station's own dollar account, everywhere it is named.
    for mut ix in [
        env.ix_pull(&home, 1),
        env.ix_push(&home, 1),
        env.ix_unwind(&home, &home.vault, WAD / 2, false),
    ] {
        replace(&mut ix, home.dollars, other.dollars);
        expect_code(env.as_vault(&home, ix), address);
    }
    let executor = env.executor.insecure_clone();
    let mut ix = env.ix_execute(
        &home,
        &executor.pubkey(),
        0,
        env.ix_venue_place(&home.authority(0), vec![]),
    );
    ix.accounts[2].pubkey = other.dollars;
    expect_code(env.send(vec![ix], &[&executor]), address);
    // The authority a venue call is signed by, and its dollar account, are
    // the ones pinned with the driver: never another station's, and never
    // the station's own dollar account.
    let wrong = station_error(StationError::WrongAuthority);
    for (at, substitute) in [
        (3, other.authority(0)),
        (3, home.station),
        (4, other.authority_dollars(0)),
        (4, home.dollars),
    ] {
        let mut ix = env.ix_execute(
            &home,
            &executor.pubkey(),
            0,
            env.ix_venue_place(&home.authority(0), vec![]),
        );
        ix.accounts[at].pubkey = substitute;
        expect_code(env.send(vec![ix], &[&executor]), wrong);
    }
    // A station is created with each driver's authority and that authority's
    // dollar account exactly where its seeds put them.
    env.open_vault("third");
    let third = env.home_of("third", &[shonin_local_driver::ID]);
    let payer = env.payer.pubkey();
    for (from, to) in [
        (third.authority(0), other.authority(0)),
        (third.authority(0), third.station),
        (third.authority_dollars(0), other.authority_dollars(0)),
        (third.authority_dollars(0), third.dollars),
    ] {
        let mut ix = env.ix_create(&third, &payer, EXIT_BAND, &[Declared::local()]);
        let at = ix
            .accounts
            .iter()
            .rposition(|meta| meta.pubkey == from)
            .unwrap();
        ix.accounts[at].pubkey = to;
        expect_code(env.as_vault(&third, ix), wrong);
    }
    // The vault's, where everything home goes.
    let stranger = env.token_account(&env.alice.pubkey());
    for mut ix in [
        env.ix_pull(&home, 1),
        env.ix_push(&home, 1),
        env.ix_unwind(&home, &home.vault, WAD / 2, false),
    ] {
        replace(&mut ix, home.vault_dollars, stranger);
        expect_code(env.as_vault(&home, ix), address);
    }
    let alice = env.alice.insecure_clone();
    env.halt_and_stop(&home);
    let mut ix = env.ix_unwind_stopped(&home, &alice.pubkey(), &[0]);
    replace(&mut ix, home.vault_dollars, stranger);
    expect_code(env.send(vec![ix], &[&alice]), address);
    assert_eq!(env.usd_balance(&alice.pubkey()), 0);
}

#[test]
fn the_vaults_account_and_program_asked_are_the_stations_own() {
    let (mut env, home) = trading();
    let other = env.new_home("other", &[Declared::local()]);
    // The other vault halted long ago: its word can't stop this station.
    env.set_view(&other, 1, 0, false);
    let mut ix = env.ix_stop(&home);
    replace(&mut ix, home.vault, other.vault);
    let payer = env.payer.insecure_clone();
    expect_code(
        env.send(vec![ix], &[&payer]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintAddress),
    );
    let mut ix = env.ix_stop(&home);
    replace(&mut ix, shonin_test_vault::ID, shonin_test_driver::ID);
    expect_code(
        env.send(vec![ix], &[&payer]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintAddress),
    );
    assert!(!env.station(&home).stopped);
}

#[test]
fn the_right_accounts_sign_and_no_others() {
    let (mut env, home) = trading();
    let executor = env.executor.insecure_clone();
    let alice = env.alice.insecure_clone();
    // The executor's key must sign its own instruction.
    let mut ix = env.ix_execute(
        &home,
        &executor.pubkey(),
        0,
        env.ix_venue_place(&home.authority(0), vec![]),
    );
    ix.accounts[0].is_signer = false;
    expect_code(
        env.send(vec![ix], &[&alice]),
        anchor_error(anchor_lang::error::ErrorCode::AccountNotSigner),
    );
    // The caller of an unwind must sign.
    let mut ix = env.ix_unwind(&home, &executor.pubkey(), WAD / 2, true);
    ix.accounts[0].is_signer = false;
    expect_code(
        env.send(vec![ix], &[&alice]),
        anchor_error(anchor_lang::error::ErrorCode::AccountNotSigner),
    );
    // The station itself never signs at the top: no transaction can carry
    // its signature, so a venue instruction naming it as signer outside the
    // guard is refused by the runtime.
    // The same for its authority at the venue, the venue account's master.
    for (master, dollars) in [
        (home.station, home.dollars),
        (home.authority(0), home.authority_dollars(0)),
    ] {
        let mut direct = env.ix_venue_withdraw(&master, &dollars, 1);
        direct.accounts[0].is_signer = false;
        expect_failure(env.send(vec![direct], &[&alice]));
    }
}

#[test]
fn no_two_stations_or_settings_share_an_address() {
    let mut seen = HashSet::new();
    assert!(seen.insert(shonin_station::config_address()));
    for i in 0..256u32 {
        let vault =
            Pubkey::find_program_address(&[b"vault", &i.to_le_bytes()], &shonin_test_vault::ID).0;
        assert!(seen.insert(shonin_station::station_address(&vault)));
    }
    let mut venue = HashSet::new();
    assert!(venue.insert(shonin_local_venue::venue_address()));
    assert!(venue.insert(shonin_local_venue::vault_address()));
    for i in 0..128u32 {
        let key = Pubkey::new_unique();
        assert!(venue.insert(shonin_local_venue::account_address(&key)));
        assert!(venue.insert(shonin_local_venue::market_address(&keccak(&[
            &i.to_le_bytes()
        ]))));
    }
}

#[test]
fn whoever_creates_an_account_pays_its_rent() {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.open_vault("home");
    let payer = env.payer.pubkey();
    let before = env.lamports(&payer);
    expect_ok(env.create_station(&home, EXIT_BAND, &[Declared::local()]));
    let station = env.svm.get_account(&home.station).unwrap();
    let dollars = env.svm.get_account(&home.dollars).unwrap();
    let home = env.home_of("home", &[shonin_local_driver::ID]);
    let authority_dollars = env.svm.get_account(&home.authority_dollars(0)).unwrap();
    // Exactly what each needs to stay, and the payer paid for all three.
    assert_eq!(
        station.lamports,
        env.svm
            .minimum_balance_for_rent_exemption(station.data.len())
    );
    assert_eq!(
        dollars.lamports,
        env.svm
            .minimum_balance_for_rent_exemption(dollars.data.len())
    );
    assert_eq!(
        authority_dollars.lamports,
        env.svm
            .minimum_balance_for_rent_exemption(authority_dollars.data.len())
    );
    assert!(
        before - env.lamports(&payer)
            >= station.lamports + dollars.lamports + authority_dollars.lamports
    );
    assert_eq!(env.station(&home).payer, payer);
    // The station's venue account is the executor's to pay for, and comes
    // back to it when it closes through the guard.
    let executor = env.executor.pubkey();
    env.bless_executor_key(executor, home.vault.to_bytes(), home.bundle);
    env.approve_executor(&home);
    let open = env.ix_venue_open(&executor, &home.authority(0));
    expect_ok(env.execute(&home, open));
    let rent = env.lamports(&shonin_local_venue::account_address(&home.authority(0)));
    let before = env.lamports(&executor);
    let close = env.ix_venue_close(&home.authority(0), &executor);
    expect_ok(env.execute(&home, close));
    // One signature's fee, paid by the executor for the transaction.
    assert_eq!(env.lamports(&executor), before + rent - 5_000);
}

#[test]
fn the_station_holds_its_rent_and_nobody_can_take_its_lamports() {
    let (mut env, home) = trading();
    // The station program owns it, so no transfer out of it is possible.
    let alice = env.alice.insecure_clone();
    let ix = anchor_lang::solana_program::system_instruction::transfer(
        &home.station,
        &alice.pubkey(),
        1,
    );
    let mut ix = ix;
    ix.accounts[0].is_signer = false;
    expect_failure(env.send(vec![ix], &[&alice]));
    // And nothing a vault, a key or a passer-by sends closes it.
    let station = env.svm.get_account(&home.station).unwrap();
    assert_eq!(station.owner, shonin_station::ID);
    assert_eq!(
        station.lamports,
        env.svm
            .minimum_balance_for_rent_exemption(station.data.len())
    );
}

#[test]
fn a_driver_is_never_handed_the_stations_signature() {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("hostile", &[Declared::test()]);
    env.start_trading(&home);
    env.set_mode(mode::SIGN_AS_STATION);
    let venue_before = env.venue_vault_balance();
    let cash = env.venue_cash(&home.authority(0));
    // Valued directly, through the vault, or in an unwind: each time the
    // driver's attempt to sign as the station fails, and nothing moves.
    let ix = env.ix_value(&home);
    let payer = env.payer.insecure_clone();
    let failed = env.send(vec![ix], &[&payer]).unwrap_err();
    assert!(
        failed
            .meta
            .logs
            .iter()
            .any(|line| line.contains("signer privilege escalated")),
        "the driver failed for another reason:\n{}",
        failed.meta.logs.join("\n")
    );
    assert!(env.unwind_as_vault(&home, WAD / 4).is_err());
    assert_eq!(env.venue_vault_balance(), venue_before);
    assert_eq!(env.venue_cash(&home.authority(0)), cash);
}

#[test]
fn time_is_the_clocks_timestamp_never_the_slot() {
    let (mut env, home) = trading();
    let mut clock = env.svm.get_sysvar::<Clock>();
    clock.slot = 1_000_000;
    env.svm.set_sysvar(&clock);
    env.halt_and_stop(&home);
    // The slot moves backwards, as a jumped local clock can leave it; the
    // band follows the timestamp alone.
    env.skip(8 * DAY);
    let mut clock = env.svm.get_sysvar::<Clock>();
    clock.slot = 3;
    env.svm.set_sysvar(&clock);
    assert_eq!(env.station(&home).stopped_band(env.now()), 2 * EXIT_BAND);
    let alice = env.alice.insecure_clone();
    ok(env.unwind_as(&home, &alice, WAD));
    assert_eq!(env.usd_balance(&home.vault), FUNDS);
}
