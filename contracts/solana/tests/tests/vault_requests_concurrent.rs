//! Requests made at once: a request's account is at its maker's address, and
//! its id, its place in the queue, is given as it is made. Two requests built
//! against the same reading of the vault both land, in the order they land,
//! and nobody can take the address another request was built for, the ones
//! the program makes for a tank fill or a queued claim included. Settlement
//! knows each request by the id it holds, so none can be skipped or stood in
//! for.

use anchor_lang::InstructionData;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// Two holders ask to leave in the same block, each having read the vault
/// first, as any two wallets do: both withdrawals are taken, and settled in
/// the order they landed.
#[test]
fn two_withdrawals_asked_against_the_same_reading_are_both_taken() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    env.deposit(&v, &alice, 1_000_000_000);
    env.deposit(&v, &bob, 1_000_000_000);
    env.price(&v);
    let a = env.shares(&v, &alice.pubkey());
    let b = env.shares(&v, &bob.pubkey());
    let count = env.vault(&v).request_count;
    // Both read the vault, then both send, bob's landing first.
    let ix_a = env.ix_request_redeem(&v, &alice.pubkey(), a, &alice.pubkey());
    let ix_b = env.ix_request_redeem(&v, &bob.pubkey(), b, &bob.pubkey());
    expect_ok(env.send(vec![ix_b], &[&bob]));
    expect_ok(env.send(vec![ix_a], &[&alice]));
    assert_eq!(env.vault(&v).request_count, count + 2);
    assert_eq!(env.request(&v, count + 1).unwrap().owner, bob.pubkey());
    assert_eq!(env.request(&v, count + 2).unwrap().owner, alice.pubkey());
    env.price(&v);
    assert_eq!(env.shares(&v, &alice.pubkey()), 0);
    assert_eq!(env.shares(&v, &bob.pubkey()), 0);
    assert!(env.usd_balance(&alice.pubkey()) > 990_000_000);
    assert!(env.usd_balance(&bob.pubkey()) > 990_000_000);
}

/// A passer-by with a few dollars of shares lands a one-unit redemption ahead
/// of a holder's withdrawal, block after block: the withdrawal is still
/// taken, as is a deposit built beside it.
#[test]
fn a_one_unit_request_landing_first_does_not_turn_a_withdrawal_away() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    let mallory = Keypair::new();
    env.svm.airdrop(&mallory.pubkey(), 1_000_000_000).unwrap();
    env.deposit(&v, &alice, 1_000_000_000);
    env.deposit(&v, &mallory, MINIMUM_DEPOSIT);
    env.price(&v);
    let shares = env.shares(&v, &alice.pubkey());
    // Alice's and bob's wallets read the vault and build their requests.
    let withdrawal = env.ix_request_redeem(&v, &alice.pubkey(), shares, &alice.pubkey());
    env.give(&bob.pubkey(), 1_000_000_000);
    let hash = env.vault(&v).restricted_hash;
    let deposit =
        env.ix_request_deposit(&v, &bob.pubkey(), &bob.pubkey(), 1_000_000_000, Some(hash));
    // Mallory's one-unit redemptions land first, each built just now.
    for _ in 0..3 {
        let grief = env.ix_request_redeem(&v, &mallory.pubkey(), 1, &mallory.pubkey());
        expect_ok(env.send(vec![grief], &[&mallory]));
    }
    expect_ok(env.send(vec![withdrawal], &[&alice]));
    expect_ok(env.send(vec![deposit], &[&bob]));
    env.price(&v);
    assert_eq!(env.shares(&v, &alice.pubkey()), 0);
    assert!(env.shares(&v, &bob.pubkey()) > 0);
}

/// Nobody makes a request at another's address: a request naming the
/// address alice's wallet built hers for is refused, and hers still lands.
#[test]
fn nobody_takes_the_address_another_request_was_built_for() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    env.deposit(&v, &alice, 1_000_000_000);
    env.deposit(&v, &bob, 1_000_000_000);
    env.price(&v);
    let withdrawal = env.ix_request_redeem(&v, &alice.pubkey(), 1, &alice.pubkey());
    let alices = withdrawal.accounts[3].pubkey;
    let mut grief = env.ix_request_redeem(&v, &bob.pubkey(), 1, &bob.pubkey());
    grief.accounts[3].pubkey = alices;
    expect_code(
        env.send(vec![grief], &[&bob]),
        vault_error(VaultError::WrongAccount),
    );
    expect_ok(env.send(vec![withdrawal], &[&alice]));
}

/// The keeper's tank fill is made at the vault's own address for the last
/// fill: a request landing first leaves it free.
#[test]
fn a_tank_fill_is_not_turned_away_by_a_request_landing_first() {
    let (mut env, v) = Env::with_vault();
    let author = env.author.insecure_clone();
    let alice = env.alice.insecure_clone();
    env.deposit_as(&v, &author, 1_000_000_000);
    env.deposit_as(&v, &alice, 1_000_000_000);
    let payer = env.payer.insecure_clone();
    for _ in 0..2 {
        let assets = env.vault(&v).tank.balance + MINIMUM_TANK;
        let ix = env.ix_author(&v, shonin_vault::instruction::SetTankKeep { assets }.data());
        expect_ok(env.as_author(ix));
        let keep = env.ix_keep(&v, &payer.pubkey(), None);
        env.redeem(&v, &alice, 1);
        expect_ok(env.send(vec![keep], &[&payer]));
        let id = env.vault(&v).request_count;
        let fill = env.request(&v, id).unwrap();
        assert_eq!(fill.kind, RequestKind::TankFill);
        assert_eq!(env.vault(&v).tank.fill_request, id);
        // Settled, the next fill's address moves on with it.
        env.price(&v);
        assert!(!env.vault(&v).tank.fill_pending);
    }
}

/// A claim the cash covers is queued at the claim's own address for the last
/// request it was queued by: a request landing first leaves it free.
#[test]
fn a_queued_claim_is_not_turned_away_by_a_request_landing_first() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.approve_executor(&v.home);
    expect_ok(env.to_station(&v, 190_000_000_000));
    let id = env.redeem(&v, &alice, a);
    env.price(&v);
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Pending);
    expect_ok(env.from_station(&v, 190_000_000_000));
    let payer = env.payer.insecure_clone();
    let queue = env.ix_settle_claim(&v, &payer.pubkey(), id);
    env.redeem(&v, &bob, 1);
    expect_ok(env.send_fitting(vec![queue], &[&payer]));
    let claim = env.claim(&v, id).unwrap();
    assert_eq!(claim.state, ClaimState::Queued);
    assert_eq!(env.request(&v, claim.request).unwrap().reference, id);
    env.price(&v);
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Paid);
}

/// Settlement takes each request by the id it holds: an empty account, or a
/// claim that isn't the request's, in place of a request not yet settled is
/// refused, so nobody can pass a request over.
#[test]
fn nothing_stands_in_for_a_request_settlement_has_not_passed() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.deposit(&v, &alice, 1_000_000_000);
    env.deposit(&v, &bob, 1_000_000_000);
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    let payer = env.payer.insecure_clone();
    let first = env.request_key(&v, 1);
    for stand_in in [Pubkey::new_unique(), claim_address(&v.vault, 1)] {
        let mut ix = env.ix_settle(&v, 0, 2);
        // Alice's request alone, without the accounts settling it takes.
        let at = ix.accounts.iter().position(|m| m.pubkey == first).unwrap();
        ix.accounts.drain(at..at + 3);
        ix.accounts.insert(at, AccountMeta::new(stand_in, false));
        expect_code(
            env.send(vec![ix], &[&payer]),
            vault_error(VaultError::WrongAccount),
        );
    }
    env.settle_all(&v);
    assert!(env.shares(&v, &alice.pubkey()) > 0);
}

/// A request done ahead of the cursor (taken through the fallback) keeps its
/// account until settlement has passed it; then its rent goes back.
#[test]
fn a_request_done_ahead_of_the_cursor_is_closed_once_settlement_passes_it() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let bob = env.bob.insecure_clone();
    env.deposit_as(&v, &alice, 1_000_000_000);
    env.deposit_as(&v, &bob, 1_000_000_000);
    let id = env.redeem(&v, &alice, 1_000);
    env.skip(DAY + SLOT);
    expect_ok(env.take_pro_rata(&v, &alice, id));
    assert!(env.request(&v, id).unwrap().done);
    let payer = env.payer.insecure_clone();
    let ix = env.ix_close_request(&v, id);
    expect_code(
        env.send(vec![ix], &[&payer]),
        vault_error(VaultError::NotYet),
    );
    env.redeem(&v, &bob, 1_000);
    env.price(&v);
    assert!(env.vault(&v).request_cursor >= id);
    let before = env.lamports(&alice.pubkey());
    let rent = env.lamports(&env.request_key(&v, id));
    let ix = env.ix_close_request(&v, id);
    expect_ok(env.send(vec![ix], &[&payer]));
    assert_eq!(env.lamports(&alice.pubkey()), before + rent);
}
