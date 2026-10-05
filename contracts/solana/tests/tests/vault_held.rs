//! A payout its receiver's dollar account can't take is held for them, so one
//! receiver never stops the queue. A CPI that fails can't be caught on
//! Solana, so the vault checks the receiver's associated dollar account
//! first (it exists, holds the dollar, is theirs, isn't frozen) and holds the
//! payout otherwise, for `collect_held`. Any other account in its place is an
//! error, so whoever sends the settlement can't choose to have a payout held.
//! The counterpart of `Vault.t.sol`'s `test_one_refused_payout_never_blocks_the_queue`
//! and `VaultProperties.t_a_refused_refund_is_held_and_the_queue_moves_on`.

use shonin_solana_tests::vault::*;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// Freezes `owner`'s associated dollar account in place, as a dollar whose
/// issuer froze it would leave it.
fn freeze_account(env: &mut Env, owner: &Pubkey) {
    let address = ata(owner, &env.mint);
    let mut account = env.svm.get_account(&address).expect("the account exists");
    account.data[108] = 2;
    env.svm.set_account(address, account).unwrap();
}

/// Closes `owner`'s associated dollar account, its balance moved away first.
fn remove_account(env: &mut Env, owner: &Keypair) {
    use anchor_spl::token::spl_token;
    let address = ata(&owner.pubkey(), &env.mint);
    let balance = env.usd_balance(&owner.pubkey());
    let sink = Keypair::new();
    let sink_account = env.token_account(&sink.pubkey());
    let mut ixs = Vec::new();
    if balance != 0 {
        ixs.push(
            spl_token::instruction::transfer(
                &spl_token::ID,
                &address,
                &sink_account,
                &owner.pubkey(),
                &[],
                balance,
            )
            .unwrap(),
        );
    }
    ixs.push(
        spl_token::instruction::close_account(
            &spl_token::ID,
            &address,
            &owner.pubkey(),
            &owner.pubkey(),
            &[],
        )
        .unwrap(),
    );
    expect_ok(env.send(ixs, &[owner]));
}

#[test]
fn one_refused_payout_never_blocks_the_queue() {
    for frozen in [false, true] {
        let (mut env, v) = Env::with_vault();
        let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
        let a = env.deposit_as(&v, &alice, 1_000_000_000);
        env.deposit_as(&v, &bob, 1_000_000_000);
        let id = env.redeem(&v, &alice, a);
        env.deposit(&v, &bob, 500_000_000);
        if frozen {
            freeze_account(&mut env, &alice.pubkey());
        } else {
            remove_account(&mut env, &alice);
        }
        env.price(&v);
        let r = env.request(&v, id).unwrap();
        assert!(r.held > 0, "nothing was held");
        assert_eq!(r.held_for, alice.pubkey());
        let count = env.vault(&v).request_count;
        assert!(
            env.request(&v, count).unwrap().done,
            "the request behind the refused one settled"
        );
        assert_eq!(env.vault(&v).held_total, r.held);
        // Held money is collected to any account of hers, and only by her.
        let ix = env.ix_collect_held(&v, &bob.pubkey(), HeldIn::Request(id));
        expect_code(
            env.send(vec![ix], &[&bob]),
            vault_error(VaultError::Nothing),
        );
        let other = Keypair::new();
        env.svm.airdrop(&other.pubkey(), 1_000_000_000).unwrap();
        let mut ix = env.ix_collect_held(&v, &alice.pubkey(), HeldIn::Request(id));
        let to = env.token_account(&other.pubkey());
        if !frozen {
            env.token_account(&alice.pubkey());
        }
        let fresh = Keypair::new();
        // A fresh account of hers, not the frozen one.
        let destination = {
            use anchor_spl::token::spl_token;
            let rent = env.svm.minimum_balance_for_rent_exemption(165);
            let payer = env.payer.insecure_clone();
            let create = anchor_lang::solana_program::system_instruction::create_account(
                &payer.pubkey(),
                &fresh.pubkey(),
                rent,
                165,
                &spl_token::ID,
            );
            let init = spl_token::instruction::initialize_account3(
                &spl_token::ID,
                &fresh.pubkey(),
                &env.mint,
                &alice.pubkey(),
            )
            .unwrap();
            expect_ok(env.send(vec![create, init], &[&payer, &fresh]));
            fresh.pubkey()
        };
        // Someone else's account as the destination is refused.
        for meta in ix.accounts.iter_mut() {
            if meta.pubkey == ata(&alice.pubkey(), &env.mint) {
                meta.pubkey = to;
            }
        }
        expect_code(
            env.send(vec![ix.clone()], &[&alice]),
            vault_error(VaultError::WrongAccount),
        );
        for meta in ix.accounts.iter_mut() {
            if meta.pubkey == to {
                meta.pubkey = destination;
            }
        }
        expect_ok(env.send(vec![ix.clone()], &[&alice]));
        assert_eq!(env.token_balance(&destination), r.held);
        assert_eq!(env.vault(&v).held_total, 0);
        expect_code(
            env.send(vec![ix], &[&alice]),
            vault_error(VaultError::Nothing),
        );
        // Collected, the request can be closed and its rent goes back.
        let payer = env.request(&v, id).unwrap().payer;
        let lamports = env.lamports(&payer);
        let rent = env.lamports(&env.request_key(&v, id));
        let ix = env.ix_close_request(&v, id);
        let sender = env.payer.insecure_clone();
        expect_ok(env.send(vec![ix], &[&sender]));
        assert_eq!(env.lamports(&payer), lamports + rent);
    }
}

#[test]
fn a_refused_refund_is_held_for_its_payer() {
    let (mut env, v) = Env::with_vault();
    let alice = env.alice.insecure_clone();
    let id = env.deposit(&v, &alice, 1_000_000_000);
    remove_account(&mut env, &alice);
    let ix = env.ix_author(&v, {
        use anchor_lang::InstructionData;
        shonin_vault::instruction::Retire {}.data()
    });
    expect_ok(env.as_author(ix));
    env.price(&v);
    let r = env.request(&v, id).unwrap();
    assert!(r.done);
    assert_eq!(r.held, 1_000_000_000);
    assert_eq!(r.held_for, r.payer);
    assert_eq!(env.vault(&v).escrow, 0);
    env.token_account(&alice.pubkey());
    let ix = env.ix_collect_held(&v, &alice.pubkey(), HeldIn::Request(id));
    expect_ok(env.send(vec![ix], &[&alice]));
    assert_eq!(env.usd_balance(&alice.pubkey()), 1_000_000_000);
}

#[test]
fn a_payout_to_an_account_that_isnt_the_receivers_is_refused_not_held() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 1_000_000_000);
    env.redeem(&v, &alice, a);
    env.to_next_slot();
    expect_ok(env.value_vault(&v));
    let cursor = env.vault(&v).request_cursor;
    let mut ix = env.ix_settle(&v, cursor, 1);
    let theirs = ata(&alice.pubkey(), &env.mint);
    let bobs = env.token_account(&bob.pubkey());
    for meta in ix.accounts.iter_mut() {
        if meta.pubkey == theirs {
            meta.pubkey = bobs;
        }
    }
    let payer = env.payer.insecure_clone();
    expect_code(
        env.send(vec![ix], &[&payer]),
        vault_error(VaultError::WrongAccount),
    );
}

#[test]
fn a_claims_cash_fee_the_author_cant_take_is_held_for_them() {
    let (mut env, v) = Env::with_vault();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    let a = env.deposit_as(&v, &alice, 100_000_000_000);
    env.deposit_as(&v, &bob, 100_000_000_000);
    env.deploy_spot(&v, 180_000_000_000);
    env.gain(&v, 20_000_000_000);
    let id = env.redeem(&v, &alice, a);
    env.price(&v);
    // The author has no dollar account at all.
    let author = env.author.insecure_clone();
    remove_account(&mut env, &author);
    expect_ok(env.settle_claim(&v, id));
    let vault = env.vault(&v);
    assert!(vault.held_author > 0, "the author's fee wasn't held");
    assert_eq!(env.claim(&v, id).unwrap().state, ClaimState::Paid);
    env.token_account(&author.pubkey());
    let ix = env.ix_collect_held(&v, &author.pubkey(), HeldIn::Fees);
    expect_ok(env.send(vec![ix], &[&author]));
    assert_eq!(env.usd_balance(&author.pubkey()), vault.held_author);
    // Nobody else collects it.
    let ix = env.ix_collect_held(&v, &bob.pubkey(), HeldIn::Fees);
    expect_code(
        env.send(vec![ix], &[&bob]),
        vault_error(VaultError::Nothing),
    );
    assert_eq!(env.vault(&v).held_author, 0);
}
