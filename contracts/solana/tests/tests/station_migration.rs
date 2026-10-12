//! Rewriting a station the first programs wrote in today's layout. The first
//! programs' drivers ended at their `addresses`; today's add each driver's
//! `position_mints` and `token_accounts` after them, so an account written
//! the old way can't be read as a `Station` at all, and every instruction
//! that loads one fails. `migrate_station` reads either layout whole, and
//! rewrites an old one in place: every value as it was, the new lists empty,
//! the account grown to exactly the new size, the payer covering only the
//! rent the larger account needs beyond what it holds. A station already in
//! today's layout is left as it is. Anything else is refused: an account the
//! station program doesn't own, one that isn't a station's record whole in
//! either layout, and one not at its own station address.
//!
//! The old accounts here are real stations, made and used by the program,
//! then written back by hand in the first layout, as a station the first
//! programs left would be found; and one built by hand whose rules use every
//! position kind the first programs knew.

use anchor_lang::prelude::Pubkey;
use anchor_lang::{
    AccountDeserialize, AccountSerialize, AnchorSerialize, Discriminator, InstructionData,
    ToAccountMetas,
};
use shonin_driver::{Class, Own, Position, Rule};
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use shonin_station::events::StationMigrated;
use shonin_station::state::{first, Layout, PositionMint, StationAddress, STATION_SEED};
use solana_account::Account;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

const SOL: u64 = 1_000_000_000;

// Building and writing records -------------------------------------------------

/// `station` as the first programs would have written it. Only for a
/// station whose drivers' new lists are empty, as every stand-in driver's
/// are: the first layout has nowhere to keep them.
fn first_of(station: &Station) -> first::Station {
    first::Station {
        vault: station.vault,
        vault_program: station.vault_program,
        vault_dollars: station.vault_dollars,
        dollars: station.dollars,
        mint: station.mint,
        bundle_hash: station.bundle_hash,
        cluster: station.cluster,
        exit_band_bps: station.exit_band_bps,
        idle: station.idle,
        sent_home: station.sent_home,
        stopped: station.stopped,
        stopped_at: station.stopped_at,
        payer: station.payer,
        bump: station.bump,
        drivers: station
            .drivers
            .iter()
            .map(|d| {
                assert!(d.position_mints.is_empty() && d.token_accounts.is_empty());
                first::Pinned {
                    name: d.name,
                    program: d.program,
                    venue: d.venue,
                    authority: d.authority,
                    authority_bump: d.authority_bump,
                    dollars: d.dollars,
                    key: d.key,
                    rules: d.rules.clone(),
                    fixed: d.fixed.clone(),
                    addresses: d.addresses.clone(),
                }
            })
            .collect(),
    }
}

/// A station's account in the first layout: the discriminator, then the
/// record.
fn first_bytes(old: &first::Station) -> Vec<u8> {
    let mut data = Station::DISCRIMINATOR.to_vec();
    old.serialize(&mut data).unwrap();
    data
}

/// A station's account in today's layout.
fn current_bytes(station: &Station) -> Vec<u8> {
    let mut data = Vec::new();
    station.try_serialize(&mut data).unwrap();
    data
}

fn rent(env: &Env, len: usize) -> u64 {
    env.svm.minimum_balance_for_rent_exemption(len)
}

fn account(env: &Env, key: &Pubkey) -> Account {
    env.svm.get_account(key).expect("the account exists")
}

/// Writes `data` at `address`, owned by `owner`, holding `lamports`.
fn put(env: &mut Env, address: Pubkey, owner: Pubkey, data: Vec<u8>, lamports: u64) {
    env.svm
        .set_account(
            address,
            Account {
                lamports,
                data,
                owner,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
}

/// Writes `data` at `address` as the station program's, holding exactly the
/// rent its size needs.
fn put_station(env: &mut Env, address: Pubkey, data: Vec<u8>) {
    let lamports = rent(env, data.len());
    put(env, address, shonin_station::ID, data, lamports);
}

/// Rewrites `home`'s station in the first layout, every value as the program
/// left it, holding exactly the rent the old size needs. Answers with the old
/// record.
fn rewrite_in_first_layout(env: &mut Env, home: &Home) -> first::Station {
    let old = first_of(&env.station(home));
    let data = first_bytes(&old);
    put_station(env, home.station, data);
    assert_is_first_layout(env, &home.station, &old);
    old
}

/// The account at `address` holds `old` in the first layout: `read_any` says
/// so, and an ordinary read as a `Station`, the one every other instruction
/// makes, fails on it.
#[track_caller]
fn assert_is_first_layout(env: &Env, address: &Pubkey, old: &first::Station) {
    let data = account(env, address).data;
    assert_eq!(data, first_bytes(old));
    assert_eq!(
        Station::read_any(&data),
        Some((Station::from(old.clone()), Layout::First))
    );
    assert!(Station::try_deserialize(&mut &data[..]).is_err());
    assert!(env.load::<Station>(address).is_none());
}

// Sending the migration -----------------------------------------------------------

fn ix_migrate(payer: &Pubkey, station: &Pubkey) -> Instruction {
    Instruction {
        program_id: shonin_station::ID,
        accounts: shonin_station::accounts::MigrateStation {
            payer: *payer,
            station: *station,
            system_program: solana_sdk_ids::system_program::ID,
        }
        .to_account_metas(None),
        data: shonin_station::instruction::MigrateStation {}.data(),
    }
}

/// A fresh wallet with ten SOL, not the fixture's fee payer, so what it pays
/// is only what the migration takes from it.
fn wallet(env: &mut Env) -> Keypair {
    let wallet = Keypair::new();
    env.svm.airdrop(&wallet.pubkey(), 10 * SOL).unwrap();
    wallet
}

/// `migrator` migrates `station`, the fixture's payer paying the fee.
fn migrate(env: &mut Env, migrator: &Keypair, station: &Pubkey) -> TxResult {
    let fee_payer = env.payer.insecure_clone();
    let ix = ix_migrate(&migrator.pubkey(), station);
    env.send(vec![ix], &[&fee_payer, migrator])
}

/// A fresh wallet migrates `station`, which holds `old` in the first layout,
/// and everything about the result is checked: today's layout, read by
/// `read_any` and by an ordinary read, every value as it was and the new
/// lists empty, exactly 8 bytes more per driver, exactly the rent the new
/// size needs (or what it held, if more), the wallet paying exactly the
/// difference, the owner the same, and one event naming the sizes.
#[track_caller]
fn migrates_exactly(env: &mut Env, station: &Pubkey, old: &first::Station) -> Station {
    let before = account(env, station);
    assert_eq!(before.data, first_bytes(old));
    let migrator = wallet(env);
    let funds = env.lamports(&migrator.pubkey());

    let meta = expect_ok(migrate(env, &migrator, station));

    let after = account(env, station);
    let expected = Station::from(old.clone());
    assert!(expected
        .drivers
        .iter()
        .all(|d| d.position_mints.is_empty() && d.token_accounts.is_empty()));
    assert_eq!(after.data, current_bytes(&expected));
    assert_eq!(after.data.len(), before.data.len() + 8 * old.drivers.len());
    assert_eq!(
        Station::read_any(&after.data),
        Some((expected.clone(), Layout::Current))
    );
    assert_eq!(
        Station::try_deserialize(&mut &after.data[..]).unwrap(),
        expected
    );
    assert_eq!(env.load::<Station>(station), Some(expected.clone()));

    let needed = rent(env, after.data.len());
    assert_eq!(after.lamports, needed.max(before.lamports));
    assert_eq!(
        funds - env.lamports(&migrator.pubkey()),
        after.lamports - before.lamports,
        "the payer paid other than what the station gained"
    );
    assert_eq!(after.owner, shonin_station::ID);
    assert_eq!(after.owner, before.owner);
    assert!(!after.executable);

    let migrated = events::<StationMigrated>(&meta.logs);
    assert_eq!(migrated.len(), 1);
    assert_eq!(migrated[0].station, *station);
    assert_eq!(migrated[0].size_before as usize, before.data.len());
    assert_eq!(migrated[0].size_after as usize, after.data.len());
    expected
}

/// A fresh wallet's migration of `station` goes through and changes nothing:
/// not the account, not the wallet's balance, and no event.
#[track_caller]
fn migrates_to_nothing(env: &mut Env, station: &Pubkey) {
    let before = account(env, station);
    let migrator = wallet(env);
    let funds = env.lamports(&migrator.pubkey());
    let meta = expect_ok(migrate(env, &migrator, station));
    assert_eq!(account(env, station), before);
    assert_eq!(env.lamports(&migrator.pubkey()), funds);
    assert!(events::<StationMigrated>(&meta.logs).is_empty());
}

/// A fresh wallet's migration of `address` is refused with `code`, and
/// changes nothing.
#[track_caller]
fn refused(env: &mut Env, address: &Pubkey, code: u32) {
    let before = env.svm.get_account(address);
    let migrator = wallet(env);
    let funds = env.lamports(&migrator.pubkey());
    expect_code(migrate(env, &migrator, address), code);
    assert_eq!(env.svm.get_account(address), before);
    assert_eq!(env.lamports(&migrator.pubkey()), funds);
}

fn not_a_station() -> u32 {
    station_error(StationError::NotAStation)
}

fn did_not_deserialize() -> u32 {
    anchor_error(anchor_lang::error::ErrorCode::AccountDidNotDeserialize)
}

// A record built by hand -------------------------------------------------------------

/// A driver as the first programs could pin it, its rules using every
/// position kind they knew, every value distinct.
fn hand_pinned(seed: u8) -> first::Pinned {
    let key = |i: u8| Pubkey::new_from_array([seed.wrapping_mul(16).wrapping_add(i); 32]);
    let at = |index: u8, own: Own| Position { index, own };
    first::Pinned {
        name: name(&format!("driver-{seed}")),
        program: key(1),
        venue: key(2),
        authority: key(3),
        authority_bump: 250 - seed,
        dollars: key(4),
        key: if seed == 0 { Pubkey::default() } else { key(5) },
        rules: vec![
            Rule {
                discriminator: vec![seed, 1, 2, 3, 4, 5, 6, 7],
                class: Class::Trade,
                positions: vec![
                    at(0, Own::Station),
                    at(1, Own::StationDollars),
                    at(2, Own::StationToken(None)),
                    at(3, Own::StationToken(Some(1))),
                ],
            },
            Rule {
                discriminator: vec![seed, 9],
                class: Class::Exit,
                positions: vec![
                    at(0, Own::StationAddress(b"margin".to_vec())),
                    at(5, Own::StationAddress(Vec::new())),
                    at(7, Own::Fixed(0)),
                    at(8, Own::Fixed(1)),
                ],
            },
            Rule {
                discriminator: Vec::new(),
                class: Class::Exit,
                positions: Vec::new(),
            },
        ],
        fixed: vec![vec![key(6), key(7)], vec![key(8)], Vec::new()],
        addresses: vec![
            StationAddress {
                prefix: b"margin".to_vec(),
                address: key(9),
            },
            StationAddress {
                prefix: Vec::new(),
                address: key(10),
            },
        ],
    }
}

/// A first-layout station of `drivers` hand-built drivers for a vault at a
/// fresh address, every field set, written at its own station address.
/// Answers with the address and the record.
fn hand_built_station(env: &mut Env, drivers: u8) -> (Pubkey, first::Station) {
    let vault = Keypair::new().pubkey();
    let (address, bump) =
        Pubkey::find_program_address(&[STATION_SEED, vault.as_ref()], &shonin_station::ID);
    let old = first::Station {
        vault,
        vault_program: shonin_test_vault::ID,
        vault_dollars: Pubkey::new_from_array([0xa1; 32]),
        dollars: Pubkey::new_from_array([0xa2; 32]),
        mint: env.mint,
        bundle_hash: [0xa3; 32],
        cluster: [0xa4; 32],
        exit_band_bps: 0x1234,
        idle: 0x0102_0304_0506_0708,
        sent_home: 0x1112_1314_1516_1718_191a_1b1c_1d1e_1f20,
        stopped: true,
        stopped_at: 0x2122_2324_2526_2728,
        payer: Pubkey::new_from_array([0xa5; 32]),
        bump,
        drivers: (0..drivers).map(hand_pinned).collect(),
    };
    put_station(env, address, first_bytes(&old));
    (address, old)
}

fn platform() -> Env {
    let mut env = Env::new();
    env.set_up_stations();
    env
}

// The layouts ---------------------------------------------------------------------

/// The first programs' position kinds are the first five of today's, so
/// their rules are written byte for byte as the first programs wrote them.
#[test]
fn the_first_programs_position_kinds_keep_their_encoding() {
    let bytes = |own: Own| {
        let mut data = Vec::new();
        own.serialize(&mut data).unwrap();
        data
    };
    assert_eq!(bytes(Own::Station), [0]);
    assert_eq!(bytes(Own::StationDollars), [1]);
    assert_eq!(bytes(Own::StationToken(None)), [2, 0]);
    assert_eq!(bytes(Own::StationToken(Some(5))), [2, 1, 5]);
    assert_eq!(
        bytes(Own::StationAddress(b"ab".to_vec())),
        [3, 2, 0, 0, 0, b'a', b'b']
    );
    assert_eq!(bytes(Own::Fixed(7)), [4, 7]);
}

// Migrating -------------------------------------------------------------------------

#[test]
fn a_first_layout_station_with_one_driver_is_rewritten_with_every_value_kept() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS / 2));
    env.approve_executor(&home);
    let old = rewrite_in_first_layout(&mut env, &home);
    assert_eq!(old.idle, FUNDS / 2);
    assert_ne!(old.drivers[0].key, Pubkey::default());
    // With one driver, today's layout is the first with the driver's two
    // empty lists, eight zero bytes, after it.
    let mut padded = first_bytes(&old);
    padded.extend([0; 8]);
    assert_eq!(padded, current_bytes(&Station::from(old.clone())));

    let migrated = migrates_exactly(&mut env, &home.station, &old);
    assert_eq!(migrated.idle, FUNDS / 2);
    assert_eq!(migrated.drivers[0].key, env.executor.pubkey());
}

#[test]
fn a_first_layout_station_with_every_driver_it_may_declare_is_rewritten_with_every_value_kept() {
    let mut env = platform();
    let home = env.new_home("full", &Declared::one_per_venue());
    expect_ok(env.pull(&home, FUNDS));
    let executor = env.executor.insecure_clone();
    for index in 0..home.drivers.len() {
        expect_ok(env.approve_key_at(&home, &executor, index, &executor.pubkey()));
        env.fund_venue_at(&home, index as u8, FUNDS / 4);
    }
    let old = rewrite_in_first_layout(&mut env, &home);
    assert_eq!(old.drivers.len(), 3);
    assert_eq!(old.idle, FUNDS / 4);

    migrates_exactly(&mut env, &home.station, &old);
}

#[test]
fn a_hand_built_station_using_every_first_position_kind_is_rewritten_byte_for_byte() {
    let mut env = platform();
    // With no drivers the two layouts are the same bytes, so such a record
    // reads as today's and is left as it is.
    let (address, old) = hand_built_station(&mut env, 0);
    assert_eq!(
        first_bytes(&old),
        current_bytes(&Station::from(old.clone()))
    );
    assert_eq!(
        Station::read_any(&account(&env, &address).data),
        Some((Station::from(old), Layout::Current))
    );
    migrates_to_nothing(&mut env, &address);

    for drivers in 1..=3 {
        let (address, old) = hand_built_station(&mut env, drivers);
        assert_is_first_layout(&env, &address, &old);
        let migrated = migrates_exactly(&mut env, &address, &old);
        // Every rule, pinned set and address as the first programs wrote it.
        for (new, old) in migrated.drivers.iter().zip(&old.drivers) {
            assert_eq!(new.rules, old.rules);
            assert_eq!(new.fixed, old.fixed);
            assert_eq!(new.addresses, old.addresses);
        }
        migrates_to_nothing(&mut env, &address);
    }
}

/// A driver named with one letter starts with bytes that, read in today's
/// layout, make a short list's length, so reading the first-layout record
/// in today's layout goes on to read lengths out of other fields. The
/// program refuses that reading without reserving room for those lengths,
/// and migrates the station.
#[test]
fn a_first_layout_station_whose_drivers_have_one_letter_names_is_rewritten() {
    let mut env = platform();
    for drivers in 2..=3 {
        let (address, mut old) = hand_built_station(&mut env, drivers);
        for (i, driver) in old.drivers.iter_mut().enumerate() {
            driver.name = [0; 32];
            driver.name[0] = b'a' + i as u8;
        }
        put_station(&mut env, address, first_bytes(&old));
        assert_is_first_layout(&env, &address, &old);
        migrates_exactly(&mut env, &address, &old);
        migrates_to_nothing(&mut env, &address);
    }
}

#[test]
fn a_stopped_station_is_rewritten_still_stopped_and_then_unwinds() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, 60_000_000_000);
    env.place(&home, vec![order(spot(), true, UNIT / 2, PRICE)]);
    env.halt_and_stop(&home);
    let stopped_at = env.station(&home).stopped_at;
    assert_ne!(stopped_at, 0);

    let old = rewrite_in_first_layout(&mut env, &home);
    assert!(old.stopped);
    let alice = env.alice.insecure_clone();
    expect_err(
        env.unwind_stopped(&home, &alice, &[0]),
        did_not_deserialize(),
    );

    let migrated = migrates_exactly(&mut env, &home.station, &old);
    assert!(migrated.stopped);
    assert_eq!(migrated.stopped_at, stopped_at);

    let outcome = ok(env.unwind_stopped(&home, &alice, &[0]));
    assert!(!outcome.blocked);
    assert!(outcome.sent_home > 40_000_000_000);
    assert_eq!(env.position(&home.authority(0), spot()), 0);
    assert_eq!(env.sent_home(&home), outcome.sent_home as u128);
    assert_eq!(env.idle(&home), 0);
}

#[test]
fn a_station_holding_more_than_rent_is_topped_up_only_by_what_it_lacks() {
    let mut env = platform();
    let (address, old) = hand_built_station(&mut env, 2);
    let data = first_bytes(&old);
    let (old_rent, new_rent) = (rent(&env, data.len()), rent(&env, data.len() + 16));
    assert!(new_rent > old_rent + 3);

    // Three lamports over the old rent: the payer pays the rest.
    put(&mut env, address, shonin_station::ID, data, old_rent + 3);
    migrates_exactly(&mut env, &address, &old);
    assert_eq!(env.lamports(&address), new_rent);

    // Already enough for the new size, and more: the payer pays nothing and
    // the station keeps every lamport.
    let (address, old) = hand_built_station(&mut env, 2);
    let data = first_bytes(&old);
    assert_eq!(rent(&env, data.len() + 16), new_rent);
    put(
        &mut env,
        address,
        shonin_station::ID,
        data,
        new_rent + 1_000,
    );
    migrates_exactly(&mut env, &address, &old);
    assert_eq!(env.lamports(&address), new_rent + 1_000);
}

#[test]
fn a_second_migration_changes_nothing() {
    let mut env = platform();
    let home = env.new_home("full", &Declared::one_per_venue());
    let old = rewrite_in_first_layout(&mut env, &home);
    migrates_exactly(&mut env, &home.station, &old);
    let after = account(&env, &home.station);
    migrates_to_nothing(&mut env, &home.station);
    migrates_to_nothing(&mut env, &home.station);
    assert_eq!(account(&env, &home.station), after);
}

#[test]
fn a_station_made_in_todays_layout_is_left_byte_for_byte_as_it_is() {
    let (mut env, home) = Env::with_station();
    let wide = env.new_home("wide", &Declared::one_per_venue());
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    for home in [&home, &wide] {
        let before = account(&env, &home.station);
        assert_eq!(
            Station::read_any(&before.data),
            Some((env.station(home), Layout::Current))
        );
        migrates_to_nothing(&mut env, &home.station);
    }

    // A current record whose drivers hold position mints and token accounts,
    // at its own station address, is left as it is too.
    let (address, old) = hand_built_station(&mut env, 3);
    let mut current = Station::from(old);
    for (i, driver) in current.drivers.iter_mut().enumerate() {
        driver.position_mints = vec![
            PositionMint {
                address: Pubkey::new_from_array([0xb0 + i as u8; 32]),
                bump: 200 + i as u8,
            };
            i + 1
        ];
        driver.token_accounts = vec![Pubkey::new_from_array([0xc0 + i as u8; 32]); i];
    }
    put_station(&mut env, address, current_bytes(&current));
    migrates_to_nothing(&mut env, &address);
    assert_eq!(env.load::<Station>(&address), Some(current));
}

// Using a migrated station ----------------------------------------------------------------

#[test]
fn a_first_layout_station_refuses_every_call_until_it_is_migrated() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS / 2);
    env.place(&home, vec![order(spot(), true, UNIT / 4, PRICE)]);
    let worth = env.value(&home);
    let executor = env.executor.insecure_clone();
    let deposit = env.ix_venue_deposit(&home.authority(0), &home.authority_dollars(0), 1_000);
    let execute = env.ix_execute_funded(&home, &executor.pubkey(), 0, deposit.clone(), 1_000);

    let old = rewrite_in_first_layout(&mut env, &home);
    expect_code(env.push(&home, 1), did_not_deserialize());
    expect_code(env.pull(&home, 1), did_not_deserialize());
    let value = env.ix_value(&home);
    let payer = env.payer.insecure_clone();
    expect_code(env.send(vec![value], &[&payer]), did_not_deserialize());
    expect_code(
        env.send_fitting(vec![execute], &[&executor]),
        did_not_deserialize(),
    );
    expect_err(env.unwind_as_vault(&home, WAD / 2), did_not_deserialize());

    migrates_exactly(&mut env, &home.station, &old);

    // The vault's push and pull.
    let idle = env.idle(&home);
    assert_eq!(idle, FUNDS / 2);
    let vault = env.usd_balance(&home.vault);
    expect_ok(env.push(&home, 1_000));
    assert_eq!(env.usd_balance(&home.vault), vault + 1_000);
    expect_ok(env.pull(&home, 1_000));
    assert_eq!(env.idle(&home), idle);
    // A value, the same as before the rewrite.
    assert_eq!(env.value(&home), worth);
    // The executor's trades through the guard, funded and not.
    expect_ok(env.execute_funded(&home, deposit, 1_000));
    env.place(&home, vec![order(spot(), true, UNIT / 8, PRICE)]);
    assert_eq!(
        env.position(&home.authority(0), spot()),
        (UNIT / 4 + UNIT / 8) as i128
    );
    assert_eq!(env.idle(&home), idle - 1_000);
    // The vault's unwind of everything.
    let (outcome, _) = ok(env.unwind_as_vault(&home, WAD));
    assert!(!outcome.blocked);
    assert_eq!(env.position(&home.authority(0), spot()), 0);
    assert_eq!(env.idle(&home), 0);
    assert_eq!(env.sent_home(&home), 1_000 + outcome.sent_home as u128);
}

#[test]
fn a_migrated_station_with_every_driver_trades_values_and_unwinds_at_every_venue() {
    let mut env = platform();
    let home = env.new_home("full", &Declared::one_per_venue());
    expect_ok(env.pull(&home, FUNDS));
    let executor = env.executor.insecure_clone();
    for index in 0..home.drivers.len() {
        expect_ok(env.approve_key_at(&home, &executor, index, &executor.pubkey()));
        env.fund_venue_at(&home, index as u8, FUNDS / 4);
    }
    let worth = env.value(&home);
    assert_eq!(worth.mark, FUNDS);

    let old = rewrite_in_first_layout(&mut env, &home);
    migrates_exactly(&mut env, &home.station, &old);

    assert_eq!(env.value(&home), worth);
    for (index, driver) in home.drivers.iter().enumerate() {
        env.place_at(
            &home,
            index as u8,
            vec![order(spot(), true, UNIT / 10, PRICE)],
        );
        let venue = venue_of(driver);
        assert_eq!(
            env.position_at(&venue, &home.authority(index), spot()),
            (UNIT / 10) as i128
        );
    }
    let unwind = env.ix_unwind(&home, &home.vault, WAD, false);
    let unwind = env.ix_call(&home, unwind, 0);
    let payer = env.payer.insecure_clone();
    expect_ok(env.send_fitting(vec![unwind], &[&payer]));
    for (index, driver) in home.drivers.iter().enumerate() {
        let venue = venue_of(driver);
        assert_eq!(env.position_at(&venue, &home.authority(index), spot()), 0);
    }
    assert_eq!(env.idle(&home), 0);
    assert!(env.sent_home(&home) > 0);
}

// Refusals ----------------------------------------------------------------------------------

#[test]
fn an_account_the_station_program_does_not_own_is_refused() {
    let (mut env, home) = Env::with_station();
    let owner = anchor_error(anchor_lang::error::ErrorCode::ConstraintOwner);

    // A wallet, an address nothing is at, and a token account.
    let wallet = wallet(&mut env);
    refused(&mut env, &wallet.pubkey(), owner);
    refused(&mut env, &Keypair::new().pubkey(), owner);
    refused(&mut env, &home.dollars, owner);

    // A whole first-layout station record at the station's own address,
    // owned by another program or by the system program.
    let old = first_of(&env.station(&home));
    for other in [shonin_test_vault::ID, solana_sdk_ids::system_program::ID] {
        let data = first_bytes(&old);
        let lamports = rent(&env, data.len());
        put(&mut env, home.station, other, data, lamports);
        refused(&mut env, &home.station, owner);
    }
}

#[test]
fn an_account_of_the_station_programs_that_is_not_a_station_is_refused() {
    let (mut env, home) = Env::with_station();
    let config = shonin_station::config_address();
    assert_eq!(account(&env, &config).owner, shonin_station::ID);
    refused(&mut env, &config, not_a_station());

    // The station's authority at its venue is a system account, not the
    // station program's.
    refused(
        &mut env,
        &home.authority(0),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintOwner),
    );

    let old = first_of(&env.station(&home));

    // Bytes after the station's discriminator that are no station's record.
    let mut garbage = Station::DISCRIMINATOR.to_vec();
    garbage.extend((0..200u8).map(|i| i.wrapping_mul(37) ^ 0x5a));
    put_station(&mut env, home.station, garbage);
    refused(&mut env, &home.station, not_a_station());

    // A first-layout record behind another account's discriminator.
    let mut other = first_bytes(&old);
    other[..8].copy_from_slice(shonin_station::state::Config::DISCRIMINATOR);
    put_station(&mut env, home.station, other);
    refused(&mut env, &home.station, not_a_station());

    // Nothing but the discriminator, and nothing at all.
    put_station(&mut env, home.station, Station::DISCRIMINATOR.to_vec());
    refused(&mut env, &home.station, not_a_station());
    put_station(&mut env, home.station, Vec::new());
    refused(&mut env, &home.station, not_a_station());
}

#[test]
fn a_record_with_bytes_past_its_end_or_cut_short_is_refused() {
    let mut env = platform();
    let home = env.new_home("full", &Declared::one_per_venue());
    let old = first_of(&env.station(&home));
    let first = first_bytes(&old);
    let current = account(&env, &home.station).data;
    for data in [first, current] {
        for extra in [0u8, 1, 0xff] {
            let mut longer = data.clone();
            longer.push(extra);
            put_station(&mut env, home.station, longer);
            refused(&mut env, &home.station, not_a_station());
        }
        for cut in [1, 8, data.len() - 9] {
            put_station(&mut env, home.station, data[..data.len() - cut].to_vec());
            refused(&mut env, &home.station, not_a_station());
        }
    }
    // The first layout with the 8 bytes per driver today's adds, zeroed,
    // past its end: with more than one driver the zeros are not where
    // today's layout keeps the new lists, so it reads whole as neither.
    let mut padded = first_bytes(&old);
    padded.extend(vec![0; 8 * old.drivers.len()]);
    put_station(&mut env, home.station, padded);
    refused(&mut env, &home.station, not_a_station());
}

#[test]
fn a_first_layout_record_not_at_its_own_station_address_is_refused() {
    let (mut env, home) = Env::with_station();
    let other = env.new_home("other", &[Declared::local()]);
    let old = first_of(&env.station(&home));

    // At a fresh address the station program owns.
    let stray = Keypair::new().pubkey();
    put_station(&mut env, stray, first_bytes(&old));
    refused(&mut env, &stray, not_a_station());

    // At another vault's station address.
    put_station(&mut env, other.station, first_bytes(&old));
    refused(&mut env, &other.station, not_a_station());

    // At its own vault's station address, its bump changed: the address its
    // vault and that bump make is another one, or none at all.
    for bump in [old.bump.wrapping_sub(1), old.bump.wrapping_add(1), 0] {
        let moved = first::Station {
            bump,
            ..old.clone()
        };
        put_station(&mut env, home.station, first_bytes(&moved));
        refused(&mut env, &home.station, not_a_station());
    }

    // At its own vault's station address, naming another vault.
    let moved = first::Station {
        vault: other.vault,
        ..old.clone()
    };
    put_station(&mut env, home.station, first_bytes(&moved));
    refused(&mut env, &home.station, not_a_station());

    // Put back as it was, it migrates.
    put_station(&mut env, home.station, first_bytes(&old));
    migrates_exactly(&mut env, &home.station, &old);
}

#[test]
fn the_payer_must_sign_and_hold_what_it_pays() {
    let (mut env, home) = Env::with_station();
    let old = rewrite_in_first_layout(&mut env, &home);

    // Named as the payer without signing.
    let victim = wallet(&mut env);
    let funds = env.lamports(&victim.pubkey());
    let mut ix = ix_migrate(&victim.pubkey(), &home.station);
    ix.accounts[0].is_signer = false;
    let payer = env.payer.insecure_clone();
    expect_code(
        env.send(vec![ix], &[&payer]),
        anchor_error(anchor_lang::error::ErrorCode::AccountNotSigner),
    );
    assert_eq!(env.lamports(&victim.pubkey()), funds);
    assert_eq!(account(&env, &home.station).data, first_bytes(&old));

    // A payer with nothing to pay: the system program's transfer fails
    // (`ResultWithNegativeLamports`).
    let broke = Keypair::new();
    let ix = ix_migrate(&broke.pubkey(), &home.station);
    expect_code(env.send(vec![ix], &[&payer, &broke]), 1);
    assert_eq!(account(&env, &home.station).data, first_bytes(&old));

    migrates_exactly(&mut env, &home.station, &old);
}
