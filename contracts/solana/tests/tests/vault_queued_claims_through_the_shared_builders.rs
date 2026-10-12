//! A queued claim's two doors, built by the instruction builders the web app
//! and the services share (`packages/chain`, `solana_ix`), and run here
//! against the vault program itself.
//!
//! When `settle_claim` queues a claim the cash covers, the program makes the
//! claim's request at the claim's own request address for the nonce the
//! claim held *before* (0 the first time) and only then stores the new
//! request's queue id in the claim. So the request a queued claim is priced
//! by is not at the address that id would give as a nonce: it is found by
//! its id, as the program finds it, and its address handed to the builders.
//! Named by that derivation instead, the program refuses both doors.

use chain::solana_ix::accounts::{Claim as SharedClaim, Request as SharedRequest};
use chain::solana_ix::vault::{claim_closed, take_pro_rata_claim, VaultKeys};
use shonin_solana_tests::station::*;
use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// The builders' keys for the vault, worked out from its account as the app
/// works them out, and checked to be the vault's.
fn keys(env: &Env, v: &V) -> VaultKeys {
    let vault = env.vault(v);
    let keys = VaultKeys::new(vault.strategy_id, vault.mint, vault.author, vault.platform);
    assert_eq!(keys.vault, v.vault, "the builders find the vault elsewhere");
    assert_eq!(keys.station, v.home.station);
    keys
}

/// Claim `id` as the shared reader reads its account.
fn shared_claim(env: &Env, v: &V, id: u64) -> SharedClaim {
    let account = env
        .svm
        .get_account(&claim_address(&v.vault, id))
        .expect("the claim's account");
    SharedClaim::read(&account.data).expect("it reads as a claim")
}

/// Where the request a queued claim is queued by lives: found by the id the
/// claim holds, and checked by the shared reader to be the claim's.
fn queued_by(env: &Env, v: &V, claim: &SharedClaim) -> Pubkey {
    let address = env.request_key(v, claim.request);
    let account = env
        .svm
        .get_account(&address)
        .expect("the request's account");
    let request = SharedRequest::read(&account.data).expect("it reads as a request");
    assert!(
        claim.is_queued_by(&v.vault, &request),
        "the request found by the claim's id is not the one it is queued by"
    );
    address
}

/// The address the claim's id would give as its request's nonce: where
/// nothing is, until the claim is queued again.
fn derived_from_the_id(v: &V, claim: &SharedClaim) -> Pubkey {
    request_address(&v.vault, &claim_address(&v.vault, claim.id), claim.request)
}

fn send(env: &mut Env, ix: Instruction, signer: &Keypair) -> TxResult {
    env.send(vec![ix], &[signer])
}

/// Alice's withdrawal of everything she holds, booked as a claim while the
/// cash was at the station, then `home` of it brought back: the claim,
/// pending.
fn a_pending_claim(env: &mut Env, v: &V, home: u64) -> u64 {
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(v, &alice, 100_000_000_000);
    env.deposit_as(v, &bob, 100_000_000_000);
    env.approve_executor(&v.home);
    expect_ok(env.to_station(v, 190_000_000_000));
    let id = env.redeem(v, &alice, a);
    env.price(v);
    assert_eq!(env.claim(v, id).unwrap().state, ClaimState::Pending);
    expect_ok(env.from_station(v, home));
    id
}

#[test]
fn a_queued_claim_s_fallback_built_by_the_shared_builders_is_taken_through_the_request_found_by_its_id(
) {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let id = a_pending_claim(&mut env, &v, 190_000_000_000);
    // The cash covers it: settling queues it, as its first request.
    expect_ok(env.settle_claim(&v, id));
    let claim = shared_claim(&env, &v, id);
    assert_eq!(claim.state, chain::solana_ix::accounts::ClaimState::Queued);
    let at = queued_by(&env, &v, &claim);
    assert_eq!(
        at,
        request_address(&v.vault, &claim_address(&v.vault, id), 0),
        "the first request is made at nonce 0"
    );
    assert_ne!(at, derived_from_the_id(&v, &claim));

    // Nothing prices it for a day: its owner takes the fallback.
    env.skip(DAY + SLOT);
    let k = keys(&env, &v);
    let wrong = derived_from_the_id(&v, &claim);
    expect_code(
        send(
            &mut env,
            take_pro_rata_claim(&k, &claim.keys(), Some(&wrong)),
            &alice,
        ),
        vault_error(VaultError::WrongAccount),
    );
    expect_ok(send(
        &mut env,
        take_pro_rata_claim(&k, &claim.keys(), Some(&at)),
        &alice,
    ));
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Fallback);
    assert_eq!(env.vault(&v).queued_claims, 0);
    assert!(env.request(&v, claim.request).unwrap().done);
    assert!(env.usd_balance(&alice.pubkey()) > 0, "nothing was paid");
}

#[test]
fn a_claim_queued_twice_is_served_through_its_second_request() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    // Just enough cash home to cover her claim.
    let id = a_pending_claim(&mut env, &v, 95_000_000_000);
    // Bob asks for a tenth of his shares back before her claim is queued:
    // his request is ahead of hers in the queue.
    let b = env.shares(&v, &bob.pubkey());
    env.redeem(&v, &bob, b / 10);
    expect_ok(env.settle_claim(&v, id));
    let first = shared_claim(&env, &v, id);
    assert_eq!(first.state, chain::solana_ix::accounts::ClaimState::Queued);
    let first_at = queued_by(&env, &v, &first);
    // Priced: Bob is paid first, and what is left no longer covers her, so
    // the claim goes back to pending, holding the first request's id.
    env.price(&v);
    let back = shared_claim(&env, &v, id);
    assert_eq!(
        back.state,
        chain::solana_ix::accounts::ClaimState::Pending,
        "the cash still covered her"
    );
    assert_eq!(back.request, first.request);
    // More cash home, and it is queued again: at the nonce it held, the
    // first request's id.
    expect_ok(env.from_station(&v, 50_000_000_000));
    expect_ok(env.settle_claim(&v, id));
    let second = shared_claim(&env, &v, id);
    assert_eq!(second.state, chain::solana_ix::accounts::ClaimState::Queued);
    assert_ne!(second.request, first.request);
    let second_at = queued_by(&env, &v, &second);
    assert_ne!(second_at, first_at);
    assert_eq!(
        second_at,
        request_address(&v.vault, &claim_address(&v.vault, id), first.request)
    );

    env.skip(DAY + SLOT);
    let k = keys(&env, &v);
    for wrong in [first_at, derived_from_the_id(&v, &second)] {
        expect_code(
            send(
                &mut env,
                take_pro_rata_claim(&k, &second.keys(), Some(&wrong)),
                &alice,
            ),
            vault_error(VaultError::WrongAccount),
        );
    }
    expect_ok(send(
        &mut env,
        take_pro_rata_claim(&k, &second.keys(), Some(&second_at)),
        &alice,
    ));
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Fallback);
}

#[test]
fn a_queued_claim_is_paid_by_the_closed_vault_through_the_request_found_by_its_id() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let id = a_pending_claim(&mut env, &v, 190_000_000_000);
    expect_ok(env.kill(&v));
    let halted = env.vault(&v).halted_at;
    env.warp(halted + HALT_GRACE);
    expect_ok(env.stop(&v.home));
    env.settle_all(&v);
    let payer = env.payer.insecure_clone();
    let unwind = env.ix_unwind(&v.home, &payer.pubkey(), WAD, false);
    expect_ok(env.send_fitting(vec![unwind], &[&payer]));
    // Queued just inside the lead of the next valuation time, so that
    // valuation doesn't price it, and the vault closes with it queued.
    env.to_boundary();
    let boundary = env.now() + SLOT;
    env.warp(boundary - 1);
    expect_ok(env.settle_claim(&v, id));
    let claim = shared_claim(&env, &v, id);
    assert_eq!(claim.state, chain::solana_ix::accounts::ClaimState::Queued);
    assert!(env.request(&v, claim.request).unwrap().earliest > boundary);
    env.warp(boundary);
    expect_ok(env.value_vault(&v));
    env.settle_all(&v);
    expect_ok(env.close_vault(&v));
    assert_eq!(env.vault(&v).state, VaultState::Closed);
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Queued);

    let k = keys(&env, &v);
    let at = queued_by(&env, &v, &claim);
    let wrong = derived_from_the_id(&v, &claim);
    expect_code(
        send(
            &mut env,
            claim_closed(&k, &claim.keys(), Some(&wrong)),
            &payer,
        ),
        vault_error(VaultError::WrongAccount),
    );
    let before = env.usd_balance(&alice.pubkey());
    expect_ok(send(
        &mut env,
        claim_closed(&k, &claim.keys(), Some(&at)),
        &payer,
    ));
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Paid);
    assert!(env.request(&v, claim.request).unwrap().done);
    assert_eq!(env.vault(&v).queued_claims, 0);
    assert!(
        env.usd_balance(&alice.pubkey()) > before,
        "nothing was paid"
    );
}
