//! Moving money, and what a station is worth. Only the vault moves dollars
//! between itself and the station, signing for its own account; the station
//! counts what arrives, sends home only what it has counted, and values
//! itself at its counted dollars plus what each pinned driver says it holds
//! at the venue. Dollars anyone sends the station's account unasked count for
//! nothing: not in its value, not in what it sends home, not at a venue.
//!
//! The station keeps a running total of everything it has sent home, raised
//! by every send on every path, whoever started it, so the vault can count
//! what came home by that total alone and never by its own balance.

use anchor_lang::prelude::Pubkey;
use shonin_solana_tests::station::*;
use shonin_solana_tests::*;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// All of the vault's money at the venue, half a BTC of it spot, nothing idle
/// at the station.
fn all_at_the_venue() -> (Env, Home) {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS);
    env.place(&home, vec![order(spot(), true, UNIT / 2, PRICE)]);
    assert_eq!(env.idle(&home), 0);
    (env, home)
}

/// Sends `ixs`, signed by `signer`, and checks that the station's total of
/// what it has sent home rose by exactly what arrived in the vault's dollar
/// account, and that its events name each send and the total after it.
/// Answers with what arrived.
fn sent_home_by(env: &mut Env, home: &Home, ixs: Vec<Instruction>, signer: &Keypair) -> u64 {
    let balance = env.usd_balance(&home.vault);
    let total = env.sent_home(home);
    let meta = expect_ok(env.send(ixs, &[signer]));
    let arrived = env.usd_balance(&home.vault) - balance;
    assert_eq!(
        env.sent_home(home) - total,
        arrived as u128,
        "the station's total rose by other than what arrived"
    );
    let events = sent_home_events(&meta.logs);
    assert_eq!(events.iter().map(|e| e.amount).sum::<u64>(), arrived);
    if let Some(last) = events.last() {
        assert_eq!(last.total, env.sent_home(home));
    }
    arrived
}

#[test]
fn the_vaults_push_and_unwind_are_counted_as_sent_home() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS / 2);
    env.place(&home, vec![order(spot(), true, UNIT / 2, PRICE)]);
    let payer = env.payer.insecure_clone();
    let push = env.ix_call(&home, env.ix_push(&home, FUNDS / 4), 0);
    assert_eq!(sent_home_by(&mut env, &home, vec![push], &payer), FUNDS / 4);
    let unwind = env.ix_unwind(&home, &home.vault, WAD / 2, false);
    let unwind = env.ix_call(&home, unwind, 0);
    assert_eq!(
        sent_home_by(&mut env, &home, vec![unwind], &payer),
        FUNDS * 3 / 8
    );
    assert_eq!(env.sent_home(&home), (FUNDS / 4 + FUNDS * 3 / 8) as u128);
}

#[test]
fn a_keys_unwind_of_a_running_station_is_counted_as_sent_home() {
    let (mut env, home) = all_at_the_venue();
    let executor = env.executor.insecure_clone();
    let ix = env.ix_unwind(&home, &executor.pubkey(), WAD / 2, true);
    assert_eq!(
        sent_home_by(&mut env, &home, vec![ix], &executor),
        FUNDS / 2
    );
    // The vault made no call and measured nothing; the total is its record.
    assert_eq!(env.received(&home), 0);
}

#[test]
fn a_strangers_unwind_of_a_stopped_station_is_counted_as_sent_home() {
    let (mut env, home) = all_at_the_venue();
    env.halt_and_stop(&home);
    let alice = env.alice.insecure_clone();
    let ix = env.ix_unwind_stopped(&home, &alice.pubkey(), &[0]);
    assert_eq!(sent_home_by(&mut env, &home, vec![ix], &alice), FUNDS);
    assert_eq!(env.sent_home(&home), FUNDS as u128);
}

#[test]
fn a_strangers_whole_unwind_of_a_stopped_station_is_counted_as_sent_home() {
    let (mut env, home) = all_at_the_venue();
    env.halt_and_stop(&home);
    let alice = env.alice.insecure_clone();
    let ix = env.ix_unwind(&home, &alice.pubkey(), WAD, false);
    assert_eq!(sent_home_by(&mut env, &home, vec![ix], &alice), FUNDS);
    assert_eq!(env.sent_home(&home), FUNDS as u128);
}

#[test]
fn a_strangers_transfer_to_the_vault_is_never_counted_as_sent_home() {
    let (mut env, home) = all_at_the_venue();
    let balance = env.usd_balance(&home.vault);
    env.give(&home.vault, FUNDS);
    assert_eq!(env.usd_balance(&home.vault), balance + FUNDS);
    assert_eq!(env.sent_home(&home), 0);
}

#[test]
fn the_vault_moves_cash_between_itself_the_station_and_the_venue() {
    let (mut env, home) = Env::with_station();
    env.start_trading(&home);
    assert_eq!(env.value(&home).mark, FUNDS);
    assert_eq!(env.usd_balance(&home.vault), 0);
    // The executor closes everything and brings it back to the station...
    let orders = vec![
        reduce(spot(), false, UNIT, PRICE),
        reduce(perp(), true, UNIT, PRICE),
    ];
    env.place(&home, orders);
    let ix = env.ix_venue_withdraw(&home.authority(0), &home.authority_dollars(0), FUNDS);
    expect_ok(env.execute(&home, ix));
    assert_eq!(env.idle(&home), FUNDS);
    // ...and the vault takes it home.
    expect_ok(env.push(&home, FUNDS));
    assert_eq!(env.usd_balance(&home.vault), FUNDS);
    assert_eq!(env.idle(&home), 0);
}

#[test]
fn only_the_vault_moves_money() {
    let (mut env, home) = Env::with_station();
    // Without the vault's signature, nobody can make the station pull or
    // push: the transaction can't carry a signature only the vault program
    // can make.
    let alice = env.alice.insecure_clone();
    for mut ix in [env.ix_pull(&home, 1), env.ix_push(&home, 1)] {
        ix.accounts[0].is_signer = false;
        expect_failure(env.send(vec![ix], &[&alice]));
    }
    // Signing as themselves instead of the vault, they name a station that
    // isn't theirs.
    for mut ix in [env.ix_pull(&home, 1), env.ix_push(&home, 1)] {
        ix.accounts[0].pubkey = alice.pubkey();
        expect_failure(env.send(vec![ix], &[&alice]));
    }
    // Another vault can't move this one's money either.
    let other = env.open_vault("other");
    let mut ix = env.ix_pull(&home, 1);
    ix.accounts[0].is_signer = false;
    let call = env.ix_call(&other, ix, 0);
    let payer = env.payer.insecure_clone();
    expect_failure(env.send(vec![call], &[&payer]));
    assert_eq!(env.usd_balance(&home.vault), FUNDS);
}

#[test]
fn the_station_sends_home_only_what_it_has_counted() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, 1_000_000));
    env.give(&home.station, 5_000_000);
    expect_code(
        env.push(&home, 1_000_001),
        station_error(StationError::NotEnoughIdle),
    );
    expect_ok(env.push(&home, 1_000_000));
    assert_eq!(env.usd_balance(&home.station), 5_000_000);
}

#[test]
fn a_strangers_dollars_count_for_nothing() {
    let (mut env, home) = Env::with_station();
    env.start_trading(&home);
    let before = env.value(&home);
    env.give(&home.station, 50_000_000_000);
    assert_eq!(env.value(&home), before, "a gift moved the station's value");
    let (outcome, _) = ok(env.unwind_as_vault(&home, WAD));
    assert_eq!(outcome.sent_home, FUNDS, "a gift was sent home");
    assert_eq!(env.usd_balance(&home.station), 50_000_000_000);
}

#[test]
fn the_station_is_worth_its_counted_dollars_and_what_its_drivers_hold() {
    let (mut env, home) = Env::with_station();
    expect_ok(env.pull(&home, FUNDS));
    env.approve_executor(&home);
    env.fund_venue(&home, FUNDS / 2);
    env.place(&home, vec![order(spot(), true, UNIT / 2, PRICE)]);
    env.set_prices(
        spot(),
        PRICE,
        PRICE,
        PRICE - 500_000_000,
        PRICE + 500_000_000,
    );
    let value = env.value(&home);
    assert_eq!(value.mark, FUNDS);
    // The half BTC counts at the bid for whoever leaves.
    assert_eq!(value.exit, FUNDS - 250_000_000);
    assert!(!value.outside);
}

#[test]
fn every_pinned_driver_must_be_asked() {
    let (mut env, home) = Env::with_station();
    env.start_trading(&home);
    let mut none = home.clone();
    none.drivers = vec![];
    let ix = env.ix_value(&none);
    let payer = env.payer.insecure_clone();
    expect_code(
        env.send(vec![ix], &[&payer]),
        station_error(StationError::WrongDrivers),
    );
    let mut twice = home.clone();
    twice.drivers = vec![shonin_local_driver::ID, shonin_local_driver::ID];
    let ix = env.ix_value(&twice);
    expect_code(
        env.send(vec![ix], &[&payer]),
        station_error(StationError::WrongDrivers),
    );
}

#[test]
fn what_the_vault_measures_arriving_is_what_the_station_sent() {
    let (mut env, home) = Env::with_station();
    env.start_trading(&home);
    for fraction in [WAD / 10, WAD / 3, WAD / 7] {
        let before = env.usd_balance(&home.vault);
        let (outcome, received) = ok(env.unwind_as_vault(&home, fraction));
        assert_eq!(received, outcome.sent_home);
        assert_eq!(env.usd_balance(&home.vault), before + received);
    }
    let _ = Pubkey::default();
}
