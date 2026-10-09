//! The Confidential Space checker: a real token's proof, made for the EVM
//! checker, is accepted by the Solana checker the registry asks, for the
//! guest and the Google keys it pins and nothing else; and the registry still
//! holds an accepted proof to its own rules (the image, the project, the
//! instance's key). A checker's settings are fixed by its address.

use anchor_lang::prelude::{AccountMeta, Pubkey};
use shonin_registry::error::RegistryError;
use shonin_solana_tests::real_token as real;
use shonin_solana_tests::*;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// The compute a blessing through the checker may use, with room: the
/// relayer asks for this much.
const BLESS_COMPUTE: u64 = 400_000;

/// The platform with a Confidential Space checker for the real guest and
/// Google's keys allowed beside the development checker, the real token's
/// image and project allowed, and the clock just after the token was issued.
fn with_checker() -> (Env, Pubkey) {
    let mut env = Env::new();
    let checker = env.create_cs_checker(real::image_id(), &real::google_keys());
    expect_ok(env.propose_cs_checker(checker));
    let admin = env.admin.insecure_clone();
    let ixs = vec![
        env.ix_propose_image(&admin.pubkey(), real::image_digest(), ImageKind::Executor),
        env.ix_propose_project(&admin.pubkey(), real::project_id()),
    ];
    expect_ok(env.send(ixs, &[&admin]));
    env.skip(TIMELOCK);
    env.warp(real::ISSUED_AT + MINUTE);
    (env, checker)
}

/// A blessing of the real token's journal under `seal`, for a key and a vault
/// of this cluster.
fn bless(env: &mut Env, journal: &Journal, seal: Vec<u8>) -> TxResult {
    let payer = env.payer.insecure_clone();
    let cluster = env.cluster();
    let ix = env.ix_bless_key(
        &payer.pubkey(),
        journal,
        seal,
        cluster,
        hash("vault"),
        hash("bundle"),
        Keypair::new().pubkey(),
        vec![],
        3,
    );
    env.send(vec![ix], &[&payer])
}

fn compute(result: &TxResult) -> u64 {
    match result {
        Ok(meta) => meta.compute_units_consumed,
        Err(failed) => failed.meta.compute_units_consumed,
    }
}

#[test]
fn a_real_token_s_proof_passes_the_checker_and_is_then_held_to_the_registry_s_rules() {
    let (mut env, _) = with_checker();
    // Its instance served no Solana cluster, so its journal names no Solana
    // key: the checker accepts the proof (anything else would be refused as
    // no proof), and the registry refuses the blessing for that.
    let result = bless(&mut env, &real::journal(), real::seal());
    let used = compute(&result);
    expect_code(result, registry_error(RegistryError::NoSigner));
    println!("a blessing through the Confidential Space checker: {used} compute units");
    assert!(used < BLESS_COMPUTE, "{used} compute units");
}

#[test]
fn an_accepted_proof_still_needs_its_image_and_project_allowed() {
    let (mut env, _) = with_checker();
    expect_ok(env.revoke_project(real::project_id()));
    expect_code(
        bless(&mut env, &real::journal(), real::seal()),
        registry_error(RegistryError::ProjectNotAllowed),
    );
    expect_ok(env.revoke_image(real::image_digest()));
    expect_code(
        bless(&mut env, &real::journal(), real::seal()),
        registry_error(RegistryError::ImageNotAllowed),
    );
}

#[test]
fn a_forged_seal_or_a_changed_journal_is_no_proof() {
    let (mut env, _) = with_checker();
    // The selector, the proof's points, the EVM key and the Google key's
    // hash carried after it.
    for at in [0, 4, 100, 259, 260, 279, 280, 311] {
        let mut seal = real::seal();
        seal[at] ^= 1;
        expect_code(
            bless(&mut env, &real::journal(), seal),
            registry_error(RegistryError::ProofRejected),
        );
    }
    let mut short = real::seal();
    short.pop();
    expect_code(
        bless(&mut env, &real::journal(), short),
        registry_error(RegistryError::ProofRejected),
    );
    // Every field of the journal: another image, project, instance key,
    // root or time.
    let honest = real::journal();
    let changed = [
        Journal {
            image_digest: hash("another image"),
            ..honest
        },
        Journal {
            project_id: real::other_project(),
            ..honest
        },
        Journal {
            signer: Keypair::new().pubkey(),
            ..honest
        },
        Journal {
            vaults_root: hash("another root"),
            ..honest
        },
        Journal {
            issued_at: honest.issued_at - 1,
            ..honest
        },
    ];
    // The checker is asked before anything else is looked at.
    for journal in changed {
        expect_code(
            bless(&mut env, &journal, real::seal()),
            registry_error(RegistryError::ProofRejected),
        );
    }
}

#[test]
fn a_checker_for_another_guest_or_other_google_keys_refuses_the_proof() {
    for (image, keys) in [
        (hash("another guest"), real::google_keys()),
        (real::image_id(), vec![real::google_keys()[1]]),
        (real::image_id(), vec![hash("a key Google never published")]),
    ] {
        let mut env = Env::new();
        let checker = env.create_cs_checker(image, &keys);
        expect_ok(env.propose_cs_checker(checker));
        let admin = env.admin.insecure_clone();
        let ixs = vec![
            env.ix_propose_image(&admin.pubkey(), real::image_digest(), ImageKind::Executor),
            env.ix_propose_project(&admin.pubkey(), real::project_id()),
        ];
        expect_ok(env.send(ixs, &[&admin]));
        env.skip(TIMELOCK);
        env.warp(real::ISSUED_AT + MINUTE);
        expect_code(
            bless(&mut env, &real::journal(), real::seal()),
            registry_error(RegistryError::ProofRejected),
        );
    }
}

#[test]
fn a_checker_counts_only_once_its_timelock_has_passed() {
    let mut env = Env::new();
    let checker = env.create_cs_checker(real::image_id(), &real::google_keys());
    expect_ok(env.propose_cs_checker(checker));
    let admin = env.admin.insecure_clone();
    let ixs = vec![
        env.ix_propose_image(&admin.pubkey(), real::image_digest(), ImageKind::Executor),
        env.ix_propose_project(&admin.pubkey(), real::project_id()),
    ];
    expect_ok(env.send(ixs, &[&admin]));
    // Before the timelock the checker isn't asked, so only the development
    // checker is, and it finds no signature of its key.
    let now = env.now();
    let config = env.config();
    let entry = config
        .verifiers
        .iter()
        .find(|entry| entry.state == checker)
        .unwrap();
    assert_eq!(entry.ready_at, now + TIMELOCK);
    assert!(!env.is_verifier_allowed(&checker));
    env.skip(TIMELOCK);
    assert!(env.is_verifier_allowed(&checker));
}

#[test]
fn the_checker_answers_by_itself_within_the_default_compute() {
    let (mut env, checker) = with_checker();
    let journal = anchor_lang::prelude::borsh::to_vec(&real::journal()).unwrap();
    let ix = Instruction {
        program_id: shonin_cs_verifier::ID,
        accounts: vec![AccountMeta::new_readonly(checker, false)],
        data: shonin_common::checker::verify_data(&real::seal(), &journal),
    };
    let payer = env.payer.insecure_clone();
    let meta = expect_ok(env.send(vec![ix], &[&payer]));
    let answer = meta.return_data.data.clone();
    assert_eq!(
        shonin_common::checker::accepted(&answer),
        Some(shonin_cs_verifier::TRUST_ROOT)
    );
    println!(
        "the checker's verify: {} compute units",
        meta.compute_units_consumed
    );
    assert!(meta.compute_units_consumed < 200_000);

    let mut forged = real::seal();
    forged[50] ^= 1;
    let ix = Instruction {
        program_id: shonin_cs_verifier::ID,
        accounts: vec![AccountMeta::new_readonly(checker, false)],
        data: shonin_common::checker::verify_data(&forged, &journal),
    };
    let meta = expect_ok(env.send(vec![ix], &[&payer]));
    assert_eq!(
        shonin_common::checker::accepted(&meta.return_data.data),
        None
    );
}

#[test]
fn a_checker_s_guest_and_keys_are_fixed_by_its_address() {
    let mut env = Env::new();
    let keys = {
        let mut keys = real::google_keys();
        keys.sort();
        keys
    };
    let checker = env.create_cs_checker(real::image_id(), &keys);
    let state: shonin_cs_verifier::Checker = env.load(&checker).unwrap();
    assert_eq!(state.image_id, real::image_id());
    assert_eq!(state.trusted_keys, keys);
    let payer = env.payer.insecure_clone();
    // The same guest and keys are the same account, made once.
    let again = env.ix_create_cs_checker(real::image_id(), keys.clone());
    expect_failure(env.send(vec![again], &[&payer]));
    // Keys out of order, repeated, zero, none or too many; no guest.
    let mut reversed = keys.clone();
    reversed.reverse();
    let refused = [
        (real::image_id(), reversed),
        (real::image_id(), vec![keys[0], keys[0]]),
        (real::image_id(), vec![[0; 32]]),
        (real::image_id(), vec![]),
        (real::image_id(), (1..=9u8).map(|i| [i; 32]).collect()),
        ([0; 32], keys.clone()),
    ];
    for (image, keys) in refused {
        let ix = env.ix_create_cs_checker(image, keys);
        expect_failure(env.send(vec![ix], &[&payer]));
    }
}

/// A blessing through the checker alone, for a key among sixteen leaves,
/// fits one transaction by address; while the development checker is still
/// allowed beside it, it goes through a lookup table, as every send too
/// large for the wire does.
#[test]
fn a_blessing_through_the_checker_fits_by_address_alone_and_through_a_table_beside_another() {
    let (mut env, _) = with_checker();
    let payer = env.payer.insecure_clone();
    let cluster = env.cluster();
    let ix = |env: &Env| {
        env.ix_bless_key(
            &payer.pubkey(),
            &real::journal(),
            real::seal(),
            cluster,
            hash("vault"),
            hash("bundle"),
            Keypair::new().pubkey(),
            vec![[7; 32]; 4],
            3,
        )
    };
    let size = env.wire_size(vec![ix(&env)], &[&payer]);
    println!("a blessing beside the development checker: {size} bytes by address");
    expect_code(
        env.send_fitting(vec![ix(&env)], &[&payer]),
        registry_error(RegistryError::NoSigner),
    );

    let dev = env.checker;
    expect_ok(env.retire_verifier(dev));
    let size = env.wire_size(vec![ix(&env)], &[&payer]);
    println!("a blessing through the checker alone: {size} bytes by address");
    assert!(size <= PACKET_DATA_SIZE, "{size} bytes");
    expect_code(
        env.send(vec![ix(&env)], &[&payer]),
        registry_error(RegistryError::NoSigner),
    );
}
