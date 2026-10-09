//! Attestation checkers rotate without a gap, retire at once, and are asked
//! in a fixed order with the accounts they were proposed with. A blessing is
//! recorded against the newest allowed checker that accepts it, so retiring
//! the old checker at the end of an overlap drops no honest key, and retiring
//! a checker found to accept forged proofs reaches every key it blessed.

use anchor_lang::prelude::{AccountMeta, Pubkey};
use anchor_lang::InstructionData;
use shonin_registry::error::RegistryError;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn vault() -> [u8; 32] {
    hash("vault")
}
fn bundle() -> [u8; 32] {
    hash("bundle")
}

/// Blesses `signer` as an executor for the test vault, sealed by `attestor`.
fn bless_executor(env: &mut Env, attestor: &Keypair, signer: Pubkey) -> TxResult {
    let journal = env.journal_for(
        executor_image(),
        signer,
        env.leaf_for(vault(), bundle(), signer),
    );
    let payer = env.payer.insecure_clone();
    let cluster = env.cluster();
    env.bless_by(
        &payer,
        attestor,
        &journal,
        cluster,
        vault(),
        bundle(),
        vec![],
    )
}

fn blessed(env: &Env, signer: &Pubkey) -> bool {
    env.is_blessed_for(signer, env.cluster(), vault(), bundle())
}

fn until(env: &Env, signer: &Pubkey) -> u64 {
    env.blessed_until(signer, env.cluster(), vault(), bundle())
}

// Proposing and retiring --------------------------------------------------------

#[test]
fn a_new_checker_waits_out_the_timelock_and_the_old_one_keeps_working() {
    let mut env = Env::new();
    let second_key = Keypair::new();
    let second = env.propose_checker(second_key.pubkey());
    assert!(!env.is_verifier_allowed(&second));

    let next = Keypair::new().pubkey();
    let journal = env.journal_for(builder_image(), next, env.service_leaf(next));
    let payer = env.payer.insecure_clone();
    expect_code(
        env.bless_by(
            &payer,
            &second_key,
            &journal,
            [0; 32],
            [0; 32],
            [0; 32],
            vec![],
        ),
        registry_error(RegistryError::ProofRejected),
    );

    env.skip(TIMELOCK);
    let journal = env.journal_for(builder_image(), next, env.service_leaf(next));
    expect_ok(env.bless_by(
        &payer,
        &second_key,
        &journal,
        [0; 32],
        [0; 32],
        [0; 32],
        vec![],
    ));
    assert!(env.is_blessed_as(&next, ImageKind::Builder));
    // The first checker still blesses.
    let another = Keypair::new().pubkey();
    env.bless_service_key(builder_image(), another);
    assert!(env.is_blessed_as(&another, ImageKind::Builder));
}

#[test]
fn retiring_a_checker_is_immediate() {
    let mut env = Env::new();
    let first = env.checker;
    expect_ok(env.retire_verifier(first));
    assert!(!env.is_verifier_allowed(&first));
    let key = Keypair::new().pubkey();
    let journal = env.journal_for(builder_image(), key, env.service_leaf(key));
    expect_code(
        env.bless(&journal, [0; 32], [0; 32], [0; 32], vec![]),
        registry_error(RegistryError::ProofRejected),
    );
}

/// A checker that doesn't accept a proof (here, one trusting another key)
/// is just a checker that didn't accept it: an older one still blesses.
#[test]
fn a_checker_that_answers_no_is_skipped() {
    let mut env = Env::new();
    let other = env.propose_checker(Keypair::new().pubkey());
    env.skip(TIMELOCK);
    assert!(env.is_verifier_allowed(&other));
    let still = Keypair::new().pubkey();
    env.bless_service_key(builder_image(), still);
    assert!(env.is_blessed_as(&still, ImageKind::Builder));
    assert_eq!(env.key_info(&still).unwrap().verifier, env.checker);
}

/// A retired checker can never be proposed again, so there is no path by
/// which every blessing it recorded comes back without a fresh seal.
#[test]
fn a_retired_checker_stays_retired() {
    let mut env = Env::new();
    let second = env.propose_checker(env.attestor.pubkey());
    env.skip(TIMELOCK);
    expect_ok(env.retire_verifier(second));
    let ix = env.ix_propose_verifier(&env.admin.pubkey(), shonin_dev_verifier::ID, None, second);
    expect_code(
        env.as_admin(ix),
        registry_error(RegistryError::AlreadyProposed),
    );
    assert!(!env.is_verifier_allowed(&second));
}

/// No admin-only path wedges a checker for good: one retired before it was
/// ever proposed can still be proposed later.
#[test]
fn a_checker_retired_before_it_was_proposed_can_still_be_proposed() {
    let mut env = Env::new();
    let next = env.create_checker(env.attestor.pubkey());
    expect_ok(env.retire_verifier(next));
    let ix = env.ix_propose_verifier(&env.admin.pubkey(), shonin_dev_verifier::ID, None, next);
    expect_ok(env.as_admin(ix.clone()));
    env.skip(TIMELOCK);
    assert!(
        env.is_verifier_allowed(&next),
        "the checker is blocked for good"
    );
    expect_code(
        env.as_admin(ix),
        registry_error(RegistryError::AlreadyProposed),
    );
}

/// A checker, like a driver or a bridge, is never the zero address:
/// otherwise it would sit in the list for good, read as allowed, and be
/// tried on every blessing.
#[test]
fn the_zero_address_cannot_be_proposed_as_a_checker() {
    let mut env = Env::new();
    let ix = env.ix_propose_verifier(
        &env.admin.pubkey(),
        shonin_dev_verifier::ID,
        None,
        Pubkey::default(),
    );
    expect_code(
        env.as_admin(ix),
        registry_error(RegistryError::NoImplementation),
    );
}

#[test]
fn the_registry_is_never_set_up_with_a_zero_checker() {
    let mut env = Env::bare();
    let deployer = env.deployer.insecure_clone();
    let ix = env.ix_initialize_registry(
        &deployer.pubkey(),
        shonin_registry::InitializeArgs {
            admin: env.admin.pubkey(),
            cluster_name: CLUSTER.to_string(),
            timelock: TIMELOCK,
            min_timelock: 0,
            blessing_ttl: BLESSING_TTL,
        },
        shonin_dev_verifier::ID,
        None,
        Pubkey::default(),
    );
    expect_code(
        env.send(vec![ix], &[&deployer]),
        registry_error(RegistryError::NoImplementation),
    );
}

/// A checker blesses every key there is, so it must be code nobody can
/// change: a program with a live upgrade authority is refused, as a driver is.
#[test]
fn a_checker_must_be_a_program_nobody_can_change() {
    let mut env = Env::new();
    let upgradeable = env.upgradeable_program(Some(env.deployer.pubkey()));
    let state = env.create_checker(env.attestor.pubkey());
    let ix = env.ix_propose_verifier(
        &env.admin.pubkey(),
        upgradeable,
        Some(program_data_address(&upgradeable)),
        state,
    );
    expect_code(env.as_admin(ix), registry_error(RegistryError::NotFinal));

    // Nor can the first checker be one.
    let mut bare = Env::bare();
    let upgradeable = bare.upgradeable_program(Some(bare.deployer.pubkey()));
    let deployer = bare.deployer.insecure_clone();
    let ix = bare.ix_initialize_registry(
        &deployer.pubkey(),
        shonin_registry::InitializeArgs {
            admin: bare.admin.pubkey(),
            cluster_name: CLUSTER.to_string(),
            timelock: TIMELOCK,
            min_timelock: 0,
            blessing_ttl: BLESSING_TTL,
        },
        upgradeable,
        Some(program_data_address(&upgradeable)),
        bare.checker,
    );
    expect_code(
        bare.send(vec![ix], &[&deployer]),
        registry_error(RegistryError::NotFinal),
    );
}

/// The state account names the checker, so it must be its program's own.
#[test]
fn a_checkers_state_must_be_its_programs_own() {
    let mut env = Env::new();
    let program = env.final_program();
    let state = env.create_checker(env.attestor.pubkey());
    let ix = env.ix_propose_verifier(&env.admin.pubkey(), program, None, state);
    expect_code(
        env.as_admin(ix),
        registry_error(RegistryError::NotAVerifier),
    );
}

/// The checkers a blessing tries are bounded; retiring one makes room.
#[test]
fn the_checkers_allowed_or_waiting_are_bounded_and_retiring_makes_room() {
    let mut env = Env::new();
    let mut proposed = Vec::new();
    for _ in 1..shonin_registry::state::MAX_VERIFIERS {
        proposed.push(env.propose_checker(Keypair::new().pubkey()));
    }
    let one_more = env.create_checker(Keypair::new().pubkey());
    let ix = env.ix_propose_verifier(&env.admin.pubkey(), shonin_dev_verifier::ID, None, one_more);
    expect_code(
        env.as_admin(ix.clone()),
        registry_error(RegistryError::TooManyVerifiers),
    );
    expect_ok(env.retire_verifier(proposed[0]));
    expect_ok(env.as_admin(ix));
    // With every slot full, a blessing still finds the checker that accepts.
    env.skip(TIMELOCK);
    let key = Keypair::new().pubkey();
    env.bless_service_key(builder_image(), key);
    assert!(env.is_blessed_as(&key, ImageKind::Builder));
}

// How a checker is asked --------------------------------------------------------

/// The caller supplies each checker's accounts, but the registry decides
/// which checkers are asked and in what order: newest first. A caller can't
/// skip the newest one to have a key recorded against an older checker.
#[test]
fn checkers_are_asked_newest_first_and_none_can_be_skipped() {
    let mut env = Env::new();
    let second = env.propose_checker(env.attestor.pubkey());
    env.skip(TIMELOCK);
    let key = Keypair::new().pubkey();
    let journal = env.journal_for(builder_image(), key, env.service_leaf(key));
    let attestor = env.attestor.insecure_clone();
    let (seal, ed25519) = env.seal(&attestor, &journal);
    let payer = env.payer.insecure_clone();

    // The first checker's accounts where the newest's belong.
    let mut ix = env.ix_bless(
        &payer.pubkey(),
        &journal,
        seal.clone(),
        [0; 32],
        [0; 32],
        [0; 32],
        vec![],
    );
    let start = ix.accounts.len() - 6;
    ix.accounts.drain(start..start + 3);
    expect_code(
        env.send(vec![ed25519.clone(), ix], &[&payer]),
        registry_error(RegistryError::WrongVerifierAccounts),
    );

    // Another checker's state account in place of the newest's.
    let mut ix = env.ix_bless(
        &payer.pubkey(),
        &journal,
        seal.clone(),
        [0; 32],
        [0; 32],
        [0; 32],
        vec![],
    );
    ix.accounts[start + 1] = AccountMeta::new_readonly(env.checker, false);
    expect_code(
        env.send(vec![ed25519.clone(), ix], &[&payer]),
        registry_error(RegistryError::WrongVerifierAccounts),
    );

    // As sent honestly, the newest records it.
    let ix = env.ix_bless(
        &payer.pubkey(),
        &journal,
        seal,
        [0; 32],
        [0; 32],
        [0; 32],
        vec![],
    );
    expect_ok(env.send(vec![ed25519, ix], &[&payer]));
    assert_eq!(env.key_info(&key).unwrap().verifier, second);
}

/// A relayer that brings no checker's accounts brings no proof.
#[test]
fn a_blessing_asks_at_least_one_checker() {
    let mut env = Env::new();
    let key = Keypair::new().pubkey();
    let journal = env.journal_for(builder_image(), key, env.service_leaf(key));
    let attestor = env.attestor.insecure_clone();
    let (_, ed25519) = env.seal(&attestor, &journal);
    let payer = env.payer.insecure_clone();
    let mut ix = env.ix_bless(
        &payer.pubkey(),
        &journal,
        vec![],
        [0; 32],
        [0; 32],
        [0; 32],
        vec![],
    );
    let groups = ix.accounts.len() - 3;
    ix.accounts.truncate(groups);
    ix.data = shonin_registry::instruction::Bless {
        journal,
        seal: vec![],
        vault_chain: [0; 32],
        vault: [0; 32],
        strategy_hash: [0; 32],
        key,
        merkle_proof: vec![],
        verifier_accounts: vec![],
    }
    .data();
    expect_code(
        env.send(vec![ed25519, ix], &[&payer]),
        registry_error(RegistryError::ProofRejected),
    );
}

// Rotation ---------------------------------------------------------------------

/// The rotation as designed: the fixed checker is ready, instances keep
/// attesting through the overlap, then the old checker is retired.
#[test]
fn retiring_the_old_checker_after_the_overlap_keeps_keys_the_new_one_accepts() {
    let mut env = Env::new();
    // The same trust root, re-implemented: accepting exactly the proofs the
    // first checker accepts.
    let fixed = env.propose_checker(env.attestor.pubkey());
    env.skip(TIMELOCK);
    let executor = env.executor.pubkey();
    let attestor = env.attestor.insecure_clone();
    expect_ok(bless_executor(&mut env, &attestor, executor));
    assert!(blessed(&env, &executor));
    assert_eq!(env.key_info(&executor).unwrap().verifier, fixed);

    expect_ok(env.retire_verifier(env.checker));
    assert!(
        blessed(&env, &executor),
        "a key the new checker accepts was dropped when the old one retired"
    );
}

/// A key blessed through the old checker alone, before the new one was
/// ready, is revived by re-sending the same proof once the new one accepts
/// it.
#[test]
fn a_proof_the_new_checker_accepts_can_be_re_blessed_after_the_old_one_retires() {
    let mut env = Env::new();
    let executor = env.executor.pubkey();
    let attestor = env.attestor.insecure_clone();
    let journal = env.journal_for(
        executor_image(),
        executor,
        env.leaf_for(vault(), bundle(), executor),
    );
    let cluster = env.cluster();
    expect_ok(env.bless(&journal, cluster, vault(), bundle(), vec![]));
    env.propose_checker(attestor.pubkey());
    env.skip(TIMELOCK);
    // The service keys were blessed before the new checker was ready, so
    // through the old one: they drop at its retirement.
    expect_ok(env.retire_verifier(env.checker));
    assert!(!env.is_blessed_as(&env.builder.pubkey(), ImageKind::Builder));
    assert!(!blessed(&env, &executor));

    // The same journal, well within its TTL, through the fixed checker.
    expect_ok(env.bless(&journal, cluster, vault(), bundle(), vec![]));
    assert!(
        blessed(&env, &executor),
        "an allowed checker vouches for the proof, but the blessing stays dead"
    );
}

#[test]
fn service_keys_survive_the_rotation_too() {
    let mut env = Env::new();
    env.propose_checker(env.attestor.pubkey());
    env.skip(TIMELOCK);
    env.skip(1);
    let (builder, reviewer) = (env.builder.pubkey(), env.reviewer.pubkey());
    env.bless_service_key(builder_image(), builder);
    env.bless_service_key(reviewer_image(), reviewer);
    expect_ok(env.retire_verifier(env.checker));
    assert!(
        env.is_blessed_as(&builder, ImageKind::Builder),
        "the builder key was dropped"
    );
    assert!(
        env.is_blessed_as(&reviewer, ImageKind::Reviewer),
        "the reviewer key was dropped"
    );
}

#[test]
fn an_older_checker_still_blesses_what_the_newest_rejects() {
    let mut env = Env::new();
    let second = env.propose_checker(env.attestor.pubkey());
    env.skip(TIMELOCK);
    env.propose_checker(Keypair::new().pubkey());
    env.skip(TIMELOCK);
    let executor = env.executor.pubkey();
    let attestor = env.attestor.insecure_clone();
    expect_ok(bless_executor(&mut env, &attestor, executor));
    assert!(blessed(&env, &executor));
    assert_eq!(env.key_info(&executor).unwrap().verifier, second);
}

// Retirement reaches every key ---------------------------------------------------

/// Whoever holds a leaked checker key attests any journal they like: the
/// image and project ids are public, and the key is their own. Retiring the
/// checker stops every key it blessed at once.
#[test]
fn retiring_a_checker_stops_the_keys_it_blessed() {
    let mut env = Env::new();
    let leaked = Keypair::new();
    let checker = env.propose_checker(leaked.pubkey());
    env.skip(TIMELOCK);
    let forged = Keypair::new().pubkey();
    expect_ok(bless_executor(&mut env, &leaked, forged));
    assert!(blessed(&env, &forged));

    expect_ok(env.retire_verifier(checker));
    assert!(!env.is_verifier_allowed(&checker));
    assert!(
        !blessed(&env, &forged),
        "a key blessed through a retired checker still counts"
    );
}

#[test]
fn retiring_a_checker_stops_the_service_keys_it_blessed() {
    let mut env = Env::new();
    let leaked = Keypair::new();
    let checker = env.propose_checker(leaked.pubkey());
    env.skip(TIMELOCK);
    let forged = Keypair::new().pubkey();
    let journal = env.journal_for(builder_image(), forged, env.service_leaf(forged));
    let payer = env.payer.insecure_clone();
    expect_ok(env.bless_by(&payer, &leaked, &journal, [0; 32], [0; 32], [0; 32], vec![]));
    assert!(env.is_blessed_as(&forged, ImageKind::Builder));
    expect_ok(env.retire_verifier(checker));
    assert!(
        !env.is_blessed_as(&forged, ImageKind::Builder),
        "a builder blessed through a retired checker can still sign registrations"
    );
}

// Revival through another checker ----------------------------------------------

/// A re-bless may move a blessing whose checker was retired. A stranger
/// holding an older public seal can use that path only while the blessing is
/// dead; once the operator has revived it with the newer seal, the older one
/// cannot shorten it.
#[test]
fn an_older_seal_cannot_shorten_a_revived_blessing() {
    let mut env = Env::new();
    let second = env.propose_checker(env.attestor.pubkey());
    env.skip(TIMELOCK);
    let executor = env.executor.pubkey();
    let cluster = env.cluster();
    let old = env.journal_for(
        executor_image(),
        executor,
        env.leaf_for(vault(), bundle(), executor),
    );
    expect_ok(env.bless(&old, cluster, vault(), bundle(), vec![]));
    let old_until = env.now() + BLESSING_TTL;

    env.skip(DAY);
    let fresh = env.journal_for(
        executor_image(),
        executor,
        env.leaf_for(vault(), bundle(), executor),
    );
    expect_ok(env.bless(&fresh, cluster, vault(), bundle(), vec![]));
    let fresh_until = env.now() + BLESSING_TTL;
    // Recorded against the newest checker.
    expect_ok(env.retire_verifier(second));
    assert!(!blessed(&env, &executor));

    // The operator revives it through the first checker.
    expect_ok(env.bless(&fresh, cluster, vault(), bundle(), vec![]));
    assert_eq!(until(&env, &executor), fresh_until);

    // A stranger's older seal does nothing to it.
    let (alice, attestor) = (env.alice.insecure_clone(), env.attestor.insecure_clone());
    expect_ok(env.bless_by(&alice, &attestor, &old, cluster, vault(), bundle(), vec![]));
    assert_eq!(until(&env, &executor), fresh_until);
    assert!(old_until < fresh_until);
    assert!(blessed(&env, &executor));
}

/// Before the revival, a stranger's older seal revives the blessing to the
/// older expiry. That is never worse than dead, and the operator's newer seal
/// still wins afterwards.
#[test]
fn a_strangers_revival_with_an_older_seal_is_overtaken_by_the_newer_one() {
    let mut env = Env::new();
    let second = env.propose_checker(env.attestor.pubkey());
    env.skip(TIMELOCK);
    let executor = env.executor.pubkey();
    let cluster = env.cluster();
    let old = env.journal_for(
        executor_image(),
        executor,
        env.leaf_for(vault(), bundle(), executor),
    );
    expect_ok(env.bless(&old, cluster, vault(), bundle(), vec![]));
    let old_until = env.now() + BLESSING_TTL;
    env.skip(DAY);
    let fresh = env.journal_for(
        executor_image(),
        executor,
        env.leaf_for(vault(), bundle(), executor),
    );
    expect_ok(env.bless(&fresh, cluster, vault(), bundle(), vec![]));
    let fresh_until = env.now() + BLESSING_TTL;
    expect_ok(env.retire_verifier(second));

    let (alice, attestor) = (env.alice.insecure_clone(), env.attestor.insecure_clone());
    expect_ok(env.bless_by(&alice, &attestor, &old, cluster, vault(), bundle(), vec![]));
    assert_eq!(until(&env, &executor), old_until);
    assert!(blessed(&env, &executor));

    expect_ok(env.bless(&fresh, cluster, vault(), bundle(), vec![]));
    assert_eq!(until(&env, &executor), fresh_until);
}

/// What a key is reported as serving follows the same rule: a stranger
/// replaying the other leaf of a two-vault journal through the
/// retired-checker path cannot re-point it away from a later blessing.
#[test]
fn key_info_follows_the_latest_blessing_even_through_a_retirement() {
    let mut env = Env::new();
    let second = env.propose_checker(env.attestor.pubkey());
    env.skip(TIMELOCK);
    let executor = env.executor.pubkey();
    let cluster = env.cluster();
    let other = hash("other vault");
    let (root, _proof_a, proof_b) = pair(
        env.leaf_for(vault(), bundle(), executor),
        env.leaf_for(other, bundle(), executor),
    );
    let journal = env.journal_for(executor_image(), executor, root);
    expect_ok(env.bless(&journal, cluster, other, bundle(), proof_b.clone()));

    env.skip(DAY);
    let fresh = env.journal_for(
        executor_image(),
        executor,
        env.leaf_for(vault(), bundle(), executor),
    );
    expect_ok(env.bless(&fresh, cluster, vault(), bundle(), vec![]));
    assert_eq!(env.key_info(&executor).unwrap().vault, vault());

    expect_ok(env.retire_verifier(second));
    // The operator revives the later blessing through the first checker.
    expect_ok(env.bless(&fresh, cluster, vault(), bundle(), vec![]));
    assert_eq!(env.key_info(&executor).unwrap().vault, vault());

    let (alice, attestor) = (env.alice.insecure_clone(), env.attestor.insecure_clone());
    expect_ok(env.bless_by(
        &alice,
        &attestor,
        &journal,
        cluster,
        other,
        bundle(),
        proof_b,
    ));
    assert_eq!(
        env.key_info(&executor).unwrap().vault,
        vault(),
        "a stranger re-pointed the key's record"
    );
}
