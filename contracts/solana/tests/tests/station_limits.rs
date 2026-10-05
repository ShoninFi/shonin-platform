//! What has to fit, at the worst case a station may be created into: as many
//! drivers as it may declare, each at its own venue, each account there
//! holding as many markets as it may. A vault's fractional close (through the
//! vault, at the depth it calls from), a key's close of a running station and
//! a stranger's full unwind of a stopped station, by either path, each go
//! through in one transaction, within the compute a transaction may ask for
//! with the caller's room to spare and the heap the station's callers ask
//! for, sent as such a transaction must be (its accounts through a lookup
//! table: three venues' accounts are more than the wire carries by address).
//! They fit too with funding owed on every position, longs and shorts both,
//! which every order, withdrawal and valuation then works out. The station's
//! record fits the room a program can give an account it creates, and is
//! valued in one transaction.

use anchor_lang::prelude::Pubkey;
use anchor_lang::{InstructionData, ToAccountMetas};
use shonin_driver::{COMPUTE_PER_DRIVER, MAX_MARKETS};
use shonin_registry::state::ImageKind;
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use shonin_station::state::{MAX_COMPUTE, MAX_CREATED, MAX_DRIVERS};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// What the worst case may take: every driver within its budget, which
/// leaves the caller its share of what a transaction may ask for.
const WITH_ROOM: u64 = MAX_DRIVERS as u64 * COMPUTE_PER_DRIVER;

/// The markets each venue lists: its spot and perp, and six more.
fn markets() -> Vec<[u8; 32]> {
    let mut markets = vec![spot(), perp()];
    markets.extend((0..6).map(|i| name(&format!("COIN-{i}"))));
    assert_eq!(markets.len(), MAX_MARKETS);
    markets
}

/// A station with a driver at every venue, as many as it may declare, its
/// account at each holding a position in every market it may hold, and a
/// little of the vault's money left idle at the station.
fn at_the_limits() -> (Env, Home) {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("full", &Declared::one_per_venue());
    assert_eq!(home.drivers.len(), MAX_DRIVERS);
    let operator = env.operator.insecure_clone();
    for venue in VENUES {
        let more: Vec<Instruction> = markets()[2..]
            .iter()
            .map(|id| env.ix_list_market_at(&venue, *id, true, PRICE))
            .collect();
        expect_ok(env.send(more, &[&operator]));
    }
    expect_ok(env.pull(&home, FUNDS));
    let executor = env.executor.insecure_clone();
    for index in 0..MAX_DRIVERS {
        expect_ok(env.approve_key_at(&home, &executor, index, &executor.pubkey()));
        env.fund_venue_at(&home, index as u8, FUNDS / 4);
        let orders: Vec<OrderRequest> = markets()
            .into_iter()
            .map(|id| {
                if id == perp() {
                    order(id, false, UNIT / 100, 0)
                } else {
                    order(id, true, UNIT / 100, PRICE)
                }
            })
            .collect();
        for batch in orders.chunks(4) {
            env.place_at(&home, index as u8, batch.to_vec());
        }
    }
    for (index, venue) in VENUES.iter().enumerate() {
        assert_eq!(
            env.held_markets_at(venue, &home.authority(index)).len(),
            MAX_MARKETS
        );
    }
    (env, home)
}

/// Every account `ixs` name, for the lookup table they are sent through.
fn table(ixs: &[Instruction]) -> Vec<Pubkey> {
    let mut keys: Vec<Pubkey> = Vec::new();
    for key in ixs
        .iter()
        .flat_map(|ix| ix.accounts.iter().map(|meta| meta.pubkey))
    {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// Sends `ix` through a lookup table and checks the compute it took.
#[track_caller]
fn fits(env: &mut Env, ix: Instruction, signer: &Keypair, what: &str) -> u64 {
    let table = table(std::slice::from_ref(&ix));
    let meta = ok(env.send_through_table(vec![ix], &[signer], &table));
    let used = meta.compute_units_consumed;
    println!("{what} at the limits: {used} compute units");
    assert!(
        used <= WITH_ROOM,
        "{what} at the limits used {used} compute units"
    );
    used
}

fn every_position(env: &Env, home: &Home) -> Vec<i128> {
    VENUES
        .iter()
        .enumerate()
        .flat_map(|(index, venue)| {
            markets()
                .into_iter()
                .map(|id| env.position_at(venue, &home.authority(index), id))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn a_vaults_close_through_every_driver_at_every_market_fits() {
    let (mut env, home) = at_the_limits();
    let before = every_position(&env, &home);
    let ix = env.ix_unwind(&home, &home.vault, WAD / 10, false);
    let call = env.ix_call(&home, ix, 0);
    let payer = env.payer.insecure_clone();
    let total = env.sent_home(&home);
    fits(&mut env, call, &payer, "a vault's fractional close");
    // Every market at every venue was closed by the fraction.
    for (after, before) in every_position(&env, &home).into_iter().zip(before) {
        assert!(after.abs() < before.abs());
    }
    assert!(env.received(&home) > 0);
    assert_eq!(env.sent_home(&home) - total, env.received(&home) as u128);
}

#[test]
fn a_strangers_full_unwind_of_a_stopped_station_at_every_market_fits() {
    let all: Vec<u8> = (0..MAX_DRIVERS as u8).collect();
    for through_named in [false, true] {
        let (mut env, home) = at_the_limits();
        env.halt_and_stop(&home);
        let alice = env.alice.insecure_clone();
        let ix = if through_named {
            env.ix_unwind_stopped(&home, &alice.pubkey(), &all)
        } else {
            env.ix_unwind(&home, &alice.pubkey(), WAD, false)
        };
        fits(&mut env, ix, &alice, "a stranger's full unwind");
        assert!(every_position(&env, &home).iter().all(|size| *size == 0));
        assert_eq!(env.idle(&home), 0);
        assert_eq!(
            env.sent_home(&home),
            env.usd_balance(&home.vault) as u128,
            "everything at the venues came home, counted"
        );
    }
}

#[test]
fn a_station_with_every_driver_it_may_declare_fits_and_is_valued_in_one_transaction() {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("full", &Declared::one_per_venue());
    let size = env.svm.get_account(&home.station).unwrap().data.len();
    assert!(size <= MAX_CREATED, "the station's record is {size} bytes");
    expect_ok(env.pull(&home, FUNDS));
    let executor = env.executor.insecure_clone();
    for index in 0..MAX_DRIVERS {
        expect_ok(env.approve_key_at(&home, &executor, index, &executor.pubkey()));
        env.fund_venue_at(&home, index as u8, FUNDS / 4);
    }
    // Each venue's account is counted once, by its own driver.
    let ix = env.ix_value(&home);
    let payer = env.payer.insecure_clone();
    let meta = expect_ok(env.send(vec![ix], &[&payer]));
    assert!(meta.compute_units_consumed < MAX_COMPUTE / 4);
    assert_eq!(env.value(&home).mark, FUNDS);
}

// With funding owed ------------------------------------------------------------

fn accrue_at(env: &Env, venue: &Pubkey, market: [u8; 32], paid_per_unit: i128) -> Instruction {
    Instruction {
        program_id: *venue,
        accounts: shonin_local_venue::accounts::Operate {
            operator: env.operator.pubkey(),
            venue: shonin_local_venue::venue_address_of(venue),
            market: shonin_local_venue::market_address_of(venue, &market),
        }
        .to_account_metas(None),
        data: shonin_local_venue::instruction::AccrueFunding { paid_per_unit }.data(),
    }
}

/// Every venue at its most markets, longs and shorts alternating, each of a
/// size no fraction divides evenly, then funding owed on every position,
/// which every order, withdrawal and valuation then has to work out.
fn at_the_limits_with_funding_owed() -> (Env, Home) {
    let mut env = Env::new();
    env.set_up_stations();
    let home = env.new_home("full", &Declared::one_per_venue());
    let operator = env.operator.insecure_clone();
    for venue in VENUES {
        let more: Vec<Instruction> = markets()[2..]
            .iter()
            .map(|id| env.ix_list_market_at(&venue, *id, true, PRICE))
            .collect();
        expect_ok(env.send(more, &[&operator]));
    }
    expect_ok(env.pull(&home, FUNDS));
    let executor = env.executor.insecure_clone();
    for index in 0..MAX_DRIVERS {
        expect_ok(env.approve_key_at(&home, &executor, index, &executor.pubkey()));
        env.fund_venue_at(&home, index as u8, FUNDS / 4);
        let orders: Vec<OrderRequest> = markets()
            .into_iter()
            .enumerate()
            .map(|(i, id)| {
                let size = UNIT / 100 + 7_777_777_777 * (i as u128 + 1);
                if id != spot() && i % 2 == 1 {
                    order(id, false, size, 0)
                } else {
                    order(id, true, size, PRICE)
                }
            })
            .collect();
        for batch in orders.chunks(4) {
            env.place_at(&home, index as u8, batch.to_vec());
        }
    }
    for (index, venue) in VENUES.into_iter().enumerate() {
        // Longs and shorts alike pay, so each venue, whose only account is
        // the station's, stays solvent.
        let accrue: Vec<Instruction> = markets()
            .iter()
            .enumerate()
            .skip(1)
            .map(|(i, id)| {
                let paid = 1_234_567 + i as i128;
                accrue_at(&env, &venue, *id, if i % 2 == 1 { -paid } else { paid })
            })
            .collect();
        for batch in accrue.chunks(4) {
            expect_ok(env.send(batch.to_vec(), &[&operator]));
        }
        assert_eq!(
            env.held_markets_at(&venue, &home.authority(index)).len(),
            MAX_MARKETS
        );
    }
    assert!(!env.value(&home).outside);
    (env, home)
}

#[test]
fn a_vaults_close_with_funding_owed_at_every_market_fits() {
    let (mut env, home) = at_the_limits_with_funding_owed();
    let ix = env.ix_unwind(&home, &home.vault, WAD / 3, false);
    let call = env.ix_call(&home, ix, 0);
    let payer = env.payer.insecure_clone();
    let total = env.sent_home(&home);
    fits(&mut env, call, &payer, "a vault's third, funding owed,");
    assert!(env.received(&home) > 0);
    assert_eq!(env.sent_home(&home) - total, env.received(&home) as u128);
}

/// A key's close of a running station reads the registry twice when the key
/// is an unwinder (as an executor first, then as an unwinder), the most a
/// close by a key reads.
#[test]
fn a_keys_close_of_a_running_station_with_funding_owed_fits() {
    let (mut env, home) = at_the_limits_with_funding_owed();
    let unwinder = Keypair::new();
    env.svm.airdrop(&unwinder.pubkey(), 1_000_000_000).unwrap();
    expect_ok(env.propose_image(unwinder_image(), ImageKind::Unwinder));
    env.skip(TIMELOCK);
    let leaf = env.leaf_for(home.vault.to_bytes(), home.bundle);
    let journal = env.journal_for(unwinder_image(), unwinder.pubkey(), leaf);
    let cluster = env.cluster();
    expect_ok(env.bless(
        &journal,
        cluster,
        home.vault.to_bytes(),
        home.bundle,
        vec![],
    ));
    let ix = env.ix_unwind(&home, &unwinder.pubkey(), WAD / 3, true);
    fits(
        &mut env,
        ix,
        &unwinder,
        "an unwinder's third, funding owed,",
    );
}

#[test]
fn a_strangers_full_unwind_with_funding_owed_at_every_market_fits() {
    let all: Vec<u8> = (0..MAX_DRIVERS as u8).collect();
    for through_named in [false, true] {
        let (mut env, home) = at_the_limits_with_funding_owed();
        env.halt_and_stop(&home);
        let alice = env.alice.insecure_clone();
        let ix = if through_named {
            env.ix_unwind_stopped(&home, &alice.pubkey(), &all)
        } else {
            env.ix_unwind(&home, &alice.pubkey(), WAD, false)
        };
        fits(
            &mut env,
            ix,
            &alice,
            "a stranger's full unwind, funding owed,",
        );
        assert!(every_position(&env, &home).iter().all(|size| *size == 0));
        assert_eq!(env.idle(&home), 0);
    }
}
