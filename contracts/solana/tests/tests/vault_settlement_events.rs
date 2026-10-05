//! A settlement says what it settled through the program's own instruction
//! data, and a valuation what it found and booked: a transaction's logs are
//! capped, and a node drops whatever comes after the cap, while instruction
//! data is kept whole. The record of a vault, a keeper learning of a claim
//! and a receiver learning a payout was held all read these.

use shonin_solana_tests::station::{events, PRICE};
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use shonin_vault::events::{
    ClaimBooked, Deposited, FeesPaid, FromStation, HoldingSynced, Transfer, ValuationVoided,
    Valued, Withdrawn,
};
use shonin_vault::state::MAX_SETTLE;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// The most withdrawals one settlement takes, each handing a fee slice over:
/// every one of their events is in the transaction's instruction data, none
/// in its logs.
#[test]
fn a_full_settlement_keeps_every_event_it_emits() {
    let (mut env, v) = Env::with_vault();
    let mut people = Vec::new();
    for _ in 0..MAX_SETTLE {
        let who = Keypair::new();
        env.svm.airdrop(&who.pubkey(), 1_000_000_000).unwrap();
        env.deposit(&v, &who, 100_000_000);
        people.push(who);
    }
    env.price(&v);
    // A gain, so each withdrawal hands its fee slice over (two more
    // `Transfer` events each).
    env.deploy_spot(&v, 100_000_000);
    env.gain(&v, 20_000_000);
    env.price(&v);
    let mut ids = Vec::new();
    for who in &people {
        let shares = env.shares(&v, &who.pubkey());
        ids.push(env.redeem(&v, who, shares / 2));
    }
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    assert!(env.vault(&v).latest.fee_exit > 0);
    let payer = env.payer.insecure_clone();
    let cursor = env.vault(&v).request_cursor;
    let ix = env.ix_settle(&v, cursor, MAX_SETTLE);
    let meta = expect_ok(env.send_fitting(vec![ix], &[&payer]));
    let vault = env.vault(&v);
    assert_eq!(vault.request_cursor, vault.priced_through);

    let withdrawn: Vec<Withdrawn> = emitted(&meta);
    assert_eq!(
        withdrawn.iter().map(|w| w.request_id).collect::<Vec<_>>(),
        ids
    );
    for (w, who) in withdrawn.iter().zip(&people) {
        assert_eq!(w.owner, who.pubkey());
        assert!(w.fee_shares > 0 && w.assets > 0);
    }
    // Each hands its slice to the author and the platform and burns the
    // rest.
    let transfers: Vec<Transfer> = emitted(&meta);
    assert_eq!(transfers.len(), 3 * MAX_SETTLE);
    let burned: u128 = transfers
        .iter()
        .filter(|t| t.from == v.vault && t.to == Pubkey::default())
        .map(|t| t.value)
        .sum();
    let fees: u128 = withdrawn.iter().map(|w| w.fee_shares).sum();
    let asked: u128 = withdrawn.iter().map(|w| w.shares).sum();
    assert_eq!(burned + fees, asked);
    // Nothing of the settlement's is left to the logs.
    assert!(events::<Withdrawn>(&meta.logs).is_empty());
    assert!(events::<Transfer>(&meta.logs).is_empty());
}

/// A deposit's shares and a withdrawal booked as a claim, said the same way.
#[test]
fn a_deposit_and_a_booked_claim_are_said_through_instruction_data() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 190_000_000_000);
    let claim = env.redeem(&v, &alice, a);
    let deposit = env.deposit(&v, &bob, 1_000_000_000);
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    let payer = env.payer.insecure_clone();
    let cursor = env.vault(&v).request_cursor;
    let ix = env.ix_settle(&v, cursor, 2);
    let meta = expect_ok(env.send_fitting(vec![ix], &[&payer]));

    let booked: Vec<ClaimBooked> = emitted(&meta);
    assert_eq!(booked.len(), 1);
    assert_eq!(booked[0].claim_id, claim);
    assert_eq!(booked[0].shares, a);
    let deposited: Vec<Deposited> = emitted(&meta);
    assert_eq!(deposited.len(), 1);
    assert_eq!(deposited[0].request_id, deposit);
    let minted: Vec<Transfer> = emitted(&meta);
    assert!(minted
        .iter()
        .any(|t| t.from == Pubkey::default() && t.to == bob.pubkey()));
    assert!(events::<Deposited>(&meta.logs).is_empty());
}

/// The executor's key closes a tenth of the running station directly: money
/// home that the vault hasn't booked yet.
fn arrival_unbooked(env: &mut Env, v: &V) -> u128 {
    let executor = env.executor.insecure_clone();
    let ix = env.ix_unwind(&v.home, &executor.pubkey(), 100_000_000_000_000_000, true);
    expect_ok(env.send_fitting(vec![ix], &[&executor]));
    let sent = env.station(&v.home).sent_home;
    assert!(sent > env.vault(v).station_booked);
    sent
}

/// What the station sent home since the vault last looked, booked by a
/// settlement, is said where the rest of the settlement is.
#[test]
fn an_arrival_booked_by_a_settlement_is_said_through_instruction_data() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 100_000_000_000);
    env.redeem(&v, &alice, a / 10);
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    assert!(env.vault(&v).owed());
    let sent = arrival_unbooked(&mut env, &v);

    let payer = env.payer.insecure_clone();
    let cursor = env.vault(&v).request_cursor;
    let ix = env.ix_settle(&v, cursor, MAX_SETTLE);
    let meta = expect_ok(env.send_fitting(vec![ix], &[&payer]));
    assert_eq!(env.vault(&v).station_booked, sent);
    let said: Vec<FromStation> = emitted(&meta);
    assert_eq!(said.len(), 1);
    assert!(said[0].assets > 0);
    assert!(events::<FromStation>(&meta.logs).is_empty());
}

/// A valuation on payday, with a profit fee owed, the holding fee accrued
/// and money home unbooked: the arrival, the holding fee's mint, the fees
/// paid and the shares minted for them are said through instruction data,
/// in the order they happened, with the valuation's own figures last.
#[test]
fn a_valuations_bookkeeping_is_said_through_instruction_data() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 100_000_000_000);
    env.gain(&v, 20_000_000_000);
    let sent = arrival_unbooked(&mut env, &v);
    env.skip(8 * DAY);
    env.to_next_slot();
    let meta = expect_ok(env.value_vault(&v));
    let vault = env.vault(&v);
    assert_eq!(vault.station_booked, sent);
    assert!(vault.priced);

    assert_eq!(emitted::<FromStation>(&meta).len(), 1);
    let holding: Vec<HoldingSynced> = emitted(&meta);
    assert_eq!(holding.len(), 1);
    let paid: Vec<FeesPaid> = emitted(&meta);
    assert_eq!(paid.len(), 1);
    assert!(paid[0].author_shares > 0 && paid[0].platform_shares > 0);
    // Payday hands over the holding fee minted since the last, this
    // valuation's with it.
    assert!(paid[0].holding_shares >= holding[0].shares);
    let author = env.author.pubkey();
    let transfers: Vec<Transfer> = emitted(&meta);
    let minted = |to: Pubkey| {
        transfers
            .iter()
            .filter(|t| t.from == Pubkey::default() && t.to == to)
            .map(|t| t.value)
            .sum::<u128>()
    };
    assert_eq!(minted(v.vault), holding[0].shares);
    assert_eq!(minted(author), paid[0].author_shares);
    assert!(transfers
        .iter()
        .any(|t| t.from == v.vault && t.value == paid[0].holding_shares));
    let valued: Vec<Valued> = emitted(&meta);
    assert_eq!(valued.len(), 1);
    assert_eq!(valued[0].index, vault.latest.index);
    // The valuation's figures come after everything it booked.
    let tag = anchor_lang::event::EVENT_IX_TAG_LE;
    let said: Vec<&[u8]> = meta
        .inner_instructions
        .iter()
        .flatten()
        .filter_map(|inner| inner.instruction.data.strip_prefix(tag))
        .collect();
    use anchor_lang::Discriminator;
    assert!(said.last().unwrap().starts_with(Valued::DISCRIMINATOR));
    assert!(said[0].starts_with(FromStation::DISCRIMINATOR));
    // Nothing of the valuation's is left to the logs.
    assert!(events::<FromStation>(&meta.logs).is_empty());
    assert!(events::<HoldingSynced>(&meta.logs).is_empty());
    assert!(events::<FeesPaid>(&meta.logs).is_empty());
    assert!(events::<Transfer>(&meta.logs).is_empty());
    assert!(events::<Valued>(&meta.logs).is_empty());
}

/// A valuation voided by a market outside the band says so the same way.
#[test]
fn a_voided_valuation_is_said_through_instruction_data() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &alice, 100_000_000_000);
    env.deploy_spot(&v, 80_000_000_000);
    env.set_spot(PRICE, 600);
    env.to_next_slot();
    let index = env.vault(&v).valuation_count;
    let meta = expect_ok(env.value_vault(&v));
    let voided: Vec<ValuationVoided> = emitted(&meta);
    assert_eq!(voided.len(), 1);
    assert_eq!(voided[0].index, index);
    assert!(emitted::<Valued>(&meta).is_empty());
    assert!(events::<ValuationVoided>(&meta.logs).is_empty());
}
