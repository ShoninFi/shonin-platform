//! A request that becomes a claim is closed into it, and from then on its
//! claim stands in for it in settlement: at the address its id gives, passed
//! in the request's place, alone. Whether it became a claim instead of a
//! withdrawal nothing priced, or was booked as one by an earlier batch of the
//! same settlement, the cursor passes it, the valuation is settled in full,
//! and the records can be closed once it has.

use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn funded(env: &mut Env) -> Keypair {
    let who = Keypair::new();
    env.svm.airdrop(&who.pubkey(), 1_000_000_000).unwrap();
    who
}

/// The dollars in the vault's own account.
fn vault_dollars(env: &Env, v: &V) -> u64 {
    env.token_balance(&v.home.vault_dollars)
}

/// A settlement from the cursor with `from` passed in place of `to` wherever
/// the keeper's accounts name it.
fn settle_swapped(env: &mut Env, v: &V, from: Pubkey, to: Pubkey) -> TxResult {
    let cursor = env.vault(v).request_cursor;
    let mut ix: Instruction = env.ix_settle(v, cursor, SETTLE_BATCH);
    let mut swapped = false;
    for meta in ix.accounts.iter_mut() {
        if meta.pubkey == from {
            meta.pubkey = to;
            swapped = true;
        }
    }
    assert!(swapped, "the settlement doesn't name {from}");
    let payer = env.payer.insecure_clone();
    env.send_fitting(vec![ix], &[&payer])
}

/// Every request the last priced valuation priced is passed.
fn assert_settled(env: &Env, v: &V) {
    let vault = env.vault(v);
    assert!(!vault.owed(), "the valuation is still owed");
    assert!(vault.request_cursor >= vault.priced_through);
}

#[test]
fn a_claim_taken_instead_of_an_unpriced_withdrawal_is_passed_by_settlement() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    // Most of the cash at the station, as a running vault holds it.
    env.deploy_spot(&v, 190_000_000_000);
    // Alice asks to leave, and a deposit follows in her slot.
    let id = env.redeem(&v, &alice, a / 2);
    let carol = funded(&mut env);
    let deposit = env.deposit(&v, &carol, MINIMUM_DEPOSIT);
    let old = env.request_key(&v, id);
    // Nothing is priced for a day; she takes a claim instead, and then its
    // fallback, so the claim is done before any valuation reaches its slot.
    env.skip(DAY + SLOT);
    let ix = env.ix_claim_instead(&v, &alice.pubkey(), id);
    expect_ok(env.send(vec![ix], &[&alice]));
    assert!(env.request(&v, id).is_none(), "the request was closed");
    let ix = env.ix_take_pro_rata_claim(&v, &alice.pubkey(), id);
    expect_ok(env.send(vec![ix], &[&alice]));
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Fallback);
    // It stands in for its request until the cursor has passed it.
    let payer = env.payer.insecure_clone();
    let ix = env.ix_close_claim(&v, id);
    expect_code(
        env.send(vec![ix], &[&payer]),
        vault_error(VaultError::NotYet),
    );

    // Valuations resume; this one prices her slot.
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    assert!(env.vault(&v).owed());
    // The request's old address, empty now, never stands in for it.
    expect_code(
        settle_swapped(&mut env, &v, claim_address(&v.vault, id), old),
        vault_error(VaultError::WrongAccount),
    );
    env.settle_all(&v);
    assert_settled(&env, &v);
    assert!(
        env.shares(&v, &carol.pubkey()) > 0,
        "the deposit was settled"
    );

    // Both records can be closed now, refunding their rent.
    let rent = env.lamports(&claim_address(&v.vault, id));
    let before = env.lamports(&alice.pubkey());
    let ix = env.ix_close_claim(&v, id);
    expect_ok(env.send(vec![ix], &[&payer]));
    assert_eq!(env.lamports(&alice.pubkey()), before + rent);
    let ix = env.ix_close_request(&v, deposit);
    expect_ok(env.send(vec![ix], &[&payer]));
    assert!(env.request(&v, deposit).is_none());
    // And the vault values and settles as ever.
    env.price(&v);
    assert_settled(&env, &v);
}

#[test]
fn a_claim_booked_in_an_earlier_batch_is_passed_by_settlement() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 190_000_000_000);
    // One slot: alice's whole withdrawal (more than the cash), then more
    // deposits than one batch takes.
    let id = env.redeem(&v, &alice, a);
    let depositors: Vec<Keypair> = (0..SETTLE_BATCH).map(|_| funded(&mut env)).collect();
    for who in &depositors {
        env.deposit(&v, who, MINIMUM_DEPOSIT);
    }
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    let vault = env.vault(&v);
    assert!(vault.priced_through - vault.request_cursor > SETTLE_BATCH as u64);

    // The first batch books her withdrawal as a claim, closing its request,
    // and can't reach the deposits behind the batch.
    assert!(env.settle_batch(&v));
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Pending);
    assert!(env.request(&v, id).is_none());
    let vault = env.vault(&v);
    assert!(vault.owed() && vault.request_cursor < id);
    // The next batch passes the claim, standing in for its request, and
    // settles the deposits.
    env.settle_all(&v);
    assert_settled(&env, &v);
    for who in &depositors {
        assert!(env.shares(&v, &who.pubkey()) > 0, "a deposit was left");
    }
    // Served, the claim can be closed.
    expect_ok(env.settle_claim(&v, id));
    let payer = env.payer.insecure_clone();
    let ix = env.ix_close_claim(&v, id);
    expect_ok(env.send(vec![ix], &[&payer]));
    env.price(&v);
    assert_settled(&env, &v);
}

#[test]
fn a_bank_run_bigger_than_one_batch_settles_paying_or_booking_everyone() {
    let (mut env, v) = Env::with_vault();
    let holders: Vec<Keypair> = (0..2 * SETTLE_BATCH + 3)
        .map(|_| funded(&mut env))
        .collect();
    let mut shares = Vec::new();
    for who in &holders {
        shares.push(env.deposit_as(&v, who, 10_000_000_000));
    }
    // Most of the cash at the station: the cash pays the first few and runs
    // out partway through the first batch.
    let cash = env.vault(&v).cash;
    env.deploy_spot(&v, cash * 7 / 10);
    let mut ids = Vec::new();
    for (who, s) in holders.iter().zip(&shares) {
        ids.push(env.redeem(&v, who, *s));
    }
    // Newcomers in the same slot, settled once every exit is.
    let newcomers: Vec<Keypair> = (0..3).map(|_| funded(&mut env)).collect();
    let mut joining = Vec::new();
    for who in &newcomers {
        joining.push(env.deposit(&v, who, MINIMUM_DEPOSIT));
    }
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    let before = env.vault(&v);
    let dollars_before = vault_dollars(&env, &v);

    // The first batch books claims; a claim of another id never stands in.
    assert!(env.settle_batch(&v));
    let booked: Vec<u64> = ids
        .iter()
        .copied()
        .filter(|id| env.claim(&v, *id).is_some())
        .collect();
    assert!(booked.len() >= 2, "the cash ran out partway: {booked:?}");
    while env.vault(&v).exit_cursor < env.vault(&v).priced_through {
        assert!(env.settle_batch(&v));
    }
    let (first, second) = (booked[0], booked[1]);
    expect_code(
        settle_swapped(
            &mut env,
            &v,
            claim_address(&v.vault, first),
            claim_address(&v.vault, second),
        ),
        vault_error(VaultError::WrongAccount),
    );
    env.settle_all(&v);
    assert_settled(&env, &v);

    // Everyone was paid in full at the valuation's price, or holds a claim
    // for every share they asked to redeem.
    let mut paid = 0u64;
    let mut claimed = 0u128;
    let mut claims = Vec::new();
    for ((who, s), id) in holders.iter().zip(&shares).zip(&ids) {
        let balance = env.usd_balance(&who.pubkey());
        match env.claim(&v, *id) {
            Some(claim) => {
                assert_eq!(claim.state, ClaimState::Pending);
                assert_eq!((claim.owner, claim.shares), (who.pubkey(), *s));
                assert_eq!(balance, 0, "a claim's owner was paid as well");
                claimed += claim.shares;
                claims.push(*id);
            }
            None => {
                let r = env.request(&v, *id).expect("the request is kept");
                assert!(r.done && r.held == 0);
                assert!(balance > 0, "a withdrawal settled without being paid");
                paid += balance;
            }
        }
    }
    let joined: u64 = newcomers.len() as u64 * MINIMUM_DEPOSIT;
    for who in &newcomers {
        assert!(env.shares(&v, &who.pubkey()) > 0, "a deposit was left");
    }
    // Cash is conserved: what left the vault's account is what was paid,
    // and its books moved by the same, the escrowed deposits joining.
    let after = env.vault(&v);
    assert_eq!(vault_dollars(&env, &v), dollars_before - paid);
    assert_eq!(after.cash, before.cash + joined - paid);
    assert_eq!(after.escrow, before.escrow - joined);
    assert_eq!(after.pending_shares, claimed);
    assert_eq!(after.claim_count - before.claim_count, claims.len() as u64);
    assert!(
        claims.len() > SETTLE_BATCH,
        "the claims span more than a batch"
    );

    // The claims are served, and every record can be closed.
    let payer = env.payer.insecure_clone();
    for id in &claims {
        expect_ok(env.settle_claim(&v, *id));
    }
    if env.vault(&v).queued_claims != 0 {
        env.price(&v);
    }
    for id in &ids {
        let ix = match env.claim(&v, *id) {
            Some(claim) => {
                assert_eq!(claim.state, ClaimState::Paid);
                env.ix_close_claim(&v, *id)
            }
            None => env.ix_close_request(&v, *id),
        };
        expect_ok(env.send(vec![ix], &[&payer]));
    }
    for id in &joining {
        let ix = env.ix_close_request(&v, *id);
        expect_ok(env.send(vec![ix], &[&payer]));
    }
    // And the vault values, settles and closes as ever.
    env.price(&v);
    assert_settled(&env, &v);
    expect_ok(env.kill(&v));
    env.wind_up(&v);
    assert_eq!(env.vault(&v).state, VaultState::Closed);
}

/// Random sequences of deposits, withdrawals, withdrawals taken as claims
/// after a day nothing was priced, valuations and settlements sent a batch at
/// a time: whatever the order, settlement always reaches the end of what the
/// last valuation priced, and the next valuation can always be recorded.
#[test]
fn settlement_always_reaches_the_priced_end() {
    let start = seed();
    let (mut instead, mut booked) = (0u64, 0u64);
    for run in 0..runs(4) {
        let mut rng = Rng::new(start + run);
        let (mut env, v) = Env::with_vault();
        let people: Vec<Keypair> = (0..4).map(|_| funded(&mut env)).collect();
        for who in &people {
            env.deposit_as(&v, who, 20_000_000_000);
        }
        // Little cash at home, so withdrawals become claims.
        let cash = env.vault(&v).cash;
        env.deploy_spot(&v, cash * 8 / 10);
        let mut redeems: Vec<u64> = Vec::new();
        for _ in 0..40 {
            let who = &people[rng.below(people.len() as u64) as usize];
            match rng.below(6) {
                0 => {
                    let assets = MINIMUM_DEPOSIT * (1 + rng.below(100));
                    env.deposit(&v, who, assets);
                }
                1 | 2 => {
                    let held = env.shares(&v, &who.pubkey());
                    if held != 0 && !env.vault(&v).ready_to_close() {
                        let part = held / (1 + rng.below(4) as u128);
                        redeems.push(env.redeem(&v, who, part.max(1)));
                    }
                }
                3 => {
                    // A day nothing is priced, then every withdrawal it left
                    // waiting may become a claim.
                    if env.vault(&v).owed() {
                        continue;
                    }
                    env.skip(DAY + SLOT);
                    for id in redeems.clone() {
                        let vault = env.vault(&v);
                        let Some(r) = env.request(&v, id) else {
                            continue;
                        };
                        let unpriced = !(vault.priced && vault.latest.time >= r.earliest);
                        if !r.done && unpriced && rng.bool() {
                            let owner = people.iter().find(|p| p.pubkey() == r.owner).unwrap();
                            let ix = env.ix_claim_instead(&v, &r.owner, id);
                            expect_ok(env.send(vec![ix], &[owner]));
                            instead += 1;
                        }
                    }
                }
                4 => {
                    if !env.vault(&v).owed() {
                        env.to_next_slot();
                        expect_ok(env.value_vault(&v));
                    }
                }
                _ => {
                    env.settle_batch(&v);
                }
            }
            let vault = env.vault(&v);
            assert!(vault.request_cursor <= vault.request_count);
            assert!(vault.exit_cursor <= vault.priced_through.max(vault.request_cursor));
        }
        env.settle_all(&v);
        assert_settled(&env, &v);
        // Whatever is left waiting is priced and settled by the next.
        env.price(&v);
        assert_settled(&env, &v);
        let vault = env.vault(&v);
        booked += vault.claim_count;
        for id in 1..=vault.priced_through {
            assert!(
                env.passable(&v, id),
                "run {run}: request {id} was passed unsettled"
            );
        }
    }
    // The sequences took claims instead and had settlement book them.
    assert!(
        instead != 0 && booked > instead,
        "{instead} of {booked} claims"
    );
}
