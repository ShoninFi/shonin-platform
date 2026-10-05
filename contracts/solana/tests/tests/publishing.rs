//! Paying to submit: the flat fee, in the dollar, from the author's own
//! account to the platform, and the next id, which belongs to whoever paid.

use anchor_lang::prelude::Pubkey;
use anchor_spl::associated_token::get_associated_token_address_with_program_id;
use anchor_spl::token::spl_token;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

#[test]
fn submitting_costs_the_fee_and_assigns_the_next_id() {
    let mut env = Env::new();
    assert_eq!(env.publishing_config().count, 0);
    let (author, alice) = (env.author.insecure_clone(), env.alice.insecure_clone());
    let first = env.submit_as(&author);
    let second = env.submit_as(&alice);
    assert_eq!(first, 1);
    assert_eq!(second, 2);
    assert_eq!(env.submission_of(first).unwrap().author, author.pubkey());
    assert_eq!(env.submission_of(second).unwrap().author, alice.pubkey());
    assert_eq!(env.submission_of(second).unwrap().paid_at, env.now());
    assert_eq!(env.usd_balance(&env.platform.pubkey()), PUBLISH_FEE * 2);
    assert_eq!(env.usd_balance(&author.pubkey()), 0);
    assert!(env.submission_of(3).is_none());
    assert!(env.submission_of(0).is_none());
    assert_eq!(env.publishing_config().count, 2);
}

#[test]
fn nobody_submits_without_paying() {
    let mut env = Env::new();
    let alice = env.alice.insecure_clone();
    env.give(&alice.pubkey(), PUBLISH_FEE - 1);
    let ix = env.ix_submit(&alice.pubkey(), hash("upload"));
    // The token program's own refusal: insufficient funds.
    expect_code(
        env.send(vec![ix], &[&alice]),
        spl_token::error::TokenError::InsufficientFunds as u32,
    );
    assert_eq!(env.publishing_config().count, 0);
    assert_eq!(env.usd_balance(&alice.pubkey()), PUBLISH_FEE - 1);
}

/// The fee comes from the sender's own account, as from the sender's balance
/// on the EVM: someone else's account, even one the sender may spend from,
/// is refused.
#[test]
fn the_fee_comes_from_the_authors_own_account() {
    let mut env = Env::new();
    let (alice, bob) = (env.alice.insecure_clone(), env.bob.insecure_clone());
    env.give(&bob.pubkey(), PUBLISH_FEE);
    let bobs =
        get_associated_token_address_with_program_id(&bob.pubkey(), &env.mint, &spl_token::ID);
    let approve = spl_token::instruction::approve(
        &spl_token::ID,
        &bobs,
        &alice.pubkey(),
        &bob.pubkey(),
        &[],
        PUBLISH_FEE,
    )
    .unwrap();
    expect_ok(env.send(vec![approve], &[&bob]));
    let mut ix = env.ix_submit(&alice.pubkey(), hash("upload"));
    ix.accounts[4].pubkey = bobs;
    expect_code(
        env.send(vec![ix], &[&alice]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintTokenOwner),
    );
    assert_eq!(env.usd_balance(&bob.pubkey()), PUBLISH_FEE);
}

/// The fee reaches the platform and nobody else.
#[test]
fn the_fee_goes_to_the_platform_alone() {
    let mut env = Env::new();
    let alice = env.alice.insecure_clone();
    env.give(&alice.pubkey(), PUBLISH_FEE);
    let thief = Keypair::new().pubkey();
    let thiefs = env.token_account(&thief);
    let mut ix = env.ix_submit(&alice.pubkey(), hash("upload"));
    ix.accounts[5].pubkey = thiefs;
    // Not the platform's: its owner is checked first.
    expect_code(
        env.send(vec![ix], &[&alice]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintTokenOwner),
    );
    // Nor a second account the platform owns but that isn't its associated
    // one.
    let platform = env.platform.pubkey();
    let other = Keypair::new();
    let rent = env.svm.minimum_balance_for_rent_exemption(165);
    let payer = env.payer.insecure_clone();
    let create = anchor_lang::solana_program::system_instruction::create_account(
        &payer.pubkey(),
        &other.pubkey(),
        rent,
        165,
        &spl_token::ID,
    );
    let init = spl_token::instruction::initialize_account3(
        &spl_token::ID,
        &other.pubkey(),
        &env.mint,
        &platform,
    )
    .unwrap();
    expect_ok(env.send(vec![create, init], &[&payer, &other]));
    let mut ix = env.ix_submit(&alice.pubkey(), hash("upload"));
    ix.accounts[5].pubkey = other.pubkey();
    expect_code(
        env.send(vec![ix], &[&alice]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintAssociated),
    );
    assert_eq!(env.usd_balance(&thief), 0);
}

/// The id's record can't be chosen: it is the next one, whatever the sender
/// names.
#[test]
fn an_id_is_the_next_one_and_nobody_chooses_it() {
    let mut env = Env::new();
    let alice = env.alice.insecure_clone();
    env.give(&alice.pubkey(), PUBLISH_FEE);
    let mut ix = env.ix_submit(&alice.pubkey(), hash("upload"));
    ix.accounts[2].pubkey = submission_address(7);
    expect_code(
        env.send(vec![ix], &[&alice]),
        anchor_error(anchor_lang::error::ErrorCode::ConstraintSeeds),
    );
    assert!(env.submission_of(7).is_none());
}

/// The submission records who paid, and the upload reference they gave.
#[test]
fn a_submission_records_who_paid_and_what_for() {
    let mut env = Env::new();
    let alice = env.alice.insecure_clone();
    env.give(&alice.pubkey(), PUBLISH_FEE);
    let ix = env.ix_submit(&alice.pubkey(), hash("my upload"));
    expect_ok(env.send(vec![ix], &[&alice]));
    let submission = env.submission_of(1).unwrap();
    assert_eq!(submission.author, alice.pubkey());
    assert_eq!(submission.upload, hash("my upload"));
    assert_eq!(submission.strategy_id, 1);
}

#[test]
fn publishing_answers_with_its_settings() {
    let env = Env::new();
    let config = env.publishing_config();
    assert_eq!(config.fee, PUBLISH_FEE);
    assert_eq!(config.mint, env.mint);
    assert_eq!(config.platform, env.platform.pubkey());
    assert_eq!(config.token_program, spl_token::ID);
    assert_eq!(config.decimals, 6);
    assert_ne!(config.platform, Pubkey::default());
}
