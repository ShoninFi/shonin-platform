//! How the development checker finds the attestor's signature,
//! and how the registry asks its checkers. Each test states what a reader
//! that followed the Ed25519 program's offsets naively, or trusted a
//! checker's accounts as sent, would have accepted, and checks the registry
//! refuses it (or, where the behaviour is accepted by design, records it).

use anchor_lang::prelude::borsh;
use anchor_lang::prelude::{AccountMeta, Pubkey};
use shonin_registry::error::RegistryError;
use shonin_registry::read as registry;
use shonin_solana_tests::*;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// An Ed25519 instruction with one signature entry laid out by hand.
fn ed25519_entry(
    sig_at: u16,
    sig_in: u16,
    key_at: u16,
    key_in: u16,
    msg_at: u16,
    msg_len: u16,
    msg_in: u16,
) -> Vec<u8> {
    let mut data = vec![1u8, 0];
    for word in [sig_at, sig_in, key_at, key_in, msg_at, msg_len, msg_in] {
        data.extend_from_slice(&word.to_le_bytes());
    }
    data
}

fn ed25519_ix(data: Vec<u8>) -> Instruction {
    Instruction {
        program_id: solana_sdk_ids::ed25519_program::ID,
        accounts: vec![],
        data,
    }
}

/// What the attestor signs for `journal` on the fixture's cluster.
fn digest_of(env: &Env, journal: &Journal) -> [u8; 32] {
    let bytes = borsh::to_vec(journal).unwrap();
    shonin_dev_verifier::attestation_digest(&env.cluster(), &bytes)
}

fn sign(signer: &Keypair, message: &[u8]) -> [u8; 64] {
    signer.sign_message(message).as_ref().try_into().unwrap()
}

fn service_journal(env: &Env) -> Journal {
    let key = Keypair::new().pubkey();
    env.journal_for(builder_image(), key, env.service_leaf(key))
}

fn bless_service(env: &Env, payer: &Pubkey, journal: &Journal, seal: Vec<u8>) -> Instruction {
    env.ix_bless(payer, journal, seal, [0; 32], [0; 32], [0; 32], vec![])
}

// The Ed25519 program's offsets -------------------------------------------------

/// The Ed25519 program lets an entry's key, signature and message live in
/// another instruction's data. The precompile verifies them there and the
/// transaction goes through, but the checker must not follow such offsets:
/// it only reads an entry whose three indexes name the Ed25519 instruction
/// itself. Here the attestor's real key, signature and digest sit inside the
/// `seal` argument of the bless instruction (index 1: the fixture sends a
/// compute-budget instruction first), and the Ed25519 instruction points at
/// them.
#[test]
fn an_entry_whose_offsets_point_into_another_instruction_is_no_proof() {
    let mut env = Env::new();
    let journal = service_journal(&env);
    let digest = digest_of(&env, &journal);
    let attestor = env.attestor.insecure_clone();
    let signature = sign(&attestor, &digest);

    let mut seal = Vec::new();
    seal.extend_from_slice(attestor.pubkey().as_ref());
    seal.extend_from_slice(&signature);
    seal.extend_from_slice(&digest);
    let payer = env.payer.insecure_clone();
    let bless = bless_service(&env, &payer.pubkey(), &journal, seal);
    // Anchor's data: the discriminator (8), the journal (136), the seal's
    // length (4), then the seal.
    let seal_at = 8 + 136 + 4;
    let bless_index = 1;
    let ed25519 = ed25519_ix(ed25519_entry(
        seal_at + 32,
        bless_index,
        seal_at,
        bless_index,
        seal_at + 96,
        32,
        bless_index,
    ));
    // The precompile accepted the transaction (the error is the registry's,
    // not a precompile failure), and the checker still answered no.
    expect_code(
        env.send(vec![bless, ed25519], &[&payer]),
        registry_error(RegistryError::ProofRejected),
    );
}

/// An entry can name the Ed25519 instruction by its own index in the
/// transaction instead of `u16::MAX`. The precompile reads the same bytes
/// either way, but the checker accepts only the `u16::MAX` form. Noted as a
/// false negative, not a hole: a relayer that builds the instruction the
/// SDK's way is never affected.
#[test]
fn an_entry_naming_its_own_instruction_by_index_is_refused_conservatively() {
    let mut env = Env::new();
    let journal = service_journal(&env);
    let digest = digest_of(&env, &journal);
    let attestor = env.attestor.insecure_clone();
    let signature = sign(&attestor, &digest);
    // Index 1 in the transaction: after the fixture's compute-budget
    // instruction, before the bless.
    let own_index = 1;
    let mut data = ed25519_entry(48, own_index, 16, own_index, 112, 32, own_index);
    data.extend_from_slice(attestor.pubkey().as_ref());
    data.extend_from_slice(&signature);
    data.extend_from_slice(&digest);
    let payer = env.payer.insecure_clone();
    let bless = bless_service(&env, &payer.pubkey(), &journal, vec![]);
    expect_code(
        env.send(vec![ed25519_ix(data), bless], &[&payer]),
        registry_error(RegistryError::ProofRejected),
    );
}

/// A signature over a prefix of the digest, or over the digest with a byte
/// after it, is a valid signature the precompile accepts, and no proof.
#[test]
fn a_signature_over_a_prefix_or_an_extension_of_the_digest_is_no_proof() {
    let mut env = Env::new();
    let journal = service_journal(&env);
    let digest = digest_of(&env, &journal);
    let attestor = env.attestor.insecure_clone();
    let payer = env.payer.insecure_clone();

    for message in [digest[..31].to_vec(), [digest.as_slice(), &[0u8]].concat()] {
        let bless = bless_service(&env, &payer.pubkey(), &journal, vec![]);
        expect_code(
            env.send(vec![Env::ed25519(&attestor, &message), bless], &[&payer]),
            registry_error(RegistryError::ProofRejected),
        );
    }
}

/// The attestor's signature over what the EVM development checker signs
/// (the journal's hash alone), or over the digest for another cluster, is
/// no proof on this cluster: purpose and cluster are in the digest.
#[test]
fn a_signature_for_another_purpose_or_cluster_is_no_proof() {
    let mut env = Env::new();
    let journal = service_journal(&env);
    let bytes = borsh::to_vec(&journal).unwrap();
    let attestor = env.attestor.insecure_clone();
    let payer = env.payer.insecure_clone();
    let devnet = shonin_dev_verifier::attestation_digest(&cluster_value("devnet"), &bytes);
    for message in [keccak(&[&bytes]), devnet] {
        let bless = bless_service(&env, &payer.pubkey(), &journal, vec![]);
        expect_code(
            env.send(vec![Env::ed25519(&attestor, &message), bless], &[&payer]),
            registry_error(RegistryError::ProofRejected),
        );
    }
}

/// Where the signature sits does not matter: after the bless instruction, or
/// as the second of two entries in one Ed25519 instruction.
#[test]
fn the_attestors_signature_counts_wherever_it_sits_in_the_transaction() {
    let mut env = Env::new();
    let attestor = env.attestor.insecure_clone();
    let payer = env.payer.insecure_clone();

    let journal = service_journal(&env);
    let bless = bless_service(&env, &payer.pubkey(), &journal, vec![]);
    let (_, ed25519) = env.seal(&attestor, &journal);
    expect_ok(env.send(vec![bless, ed25519], &[&payer]));
    assert!(env.is_blessed_as(&journal.signer, ImageKind::Builder));

    // Two entries: a stranger's over anything, then the attestor's.
    let journal = service_journal(&env);
    let digest = digest_of(&env, &journal);
    let stranger = Keypair::new();
    let noise = sign(&stranger, b"noise");
    let signature = sign(&attestor, &digest);
    let mut data = vec![2u8, 0];
    let base = 2 + 2 * 14;
    let first_key = base;
    let first_sig = first_key + 32;
    let first_msg = first_sig + 64;
    let second_key = first_msg + 5;
    let second_sig = second_key + 32;
    let second_msg = second_sig + 64;
    for (sig_at, key_at, msg_at, len) in [
        (first_sig, first_key, first_msg, 5u16),
        (second_sig, second_key, second_msg, 32),
    ] {
        for word in [sig_at, u16::MAX, key_at, u16::MAX, msg_at, len, u16::MAX] {
            data.extend_from_slice(&word.to_le_bytes());
        }
    }
    data.extend_from_slice(stranger.pubkey().as_ref());
    data.extend_from_slice(&noise);
    data.extend_from_slice(b"noise");
    data.extend_from_slice(attestor.pubkey().as_ref());
    data.extend_from_slice(&signature);
    data.extend_from_slice(&digest);
    let bless = bless_service(&env, &payer.pubkey(), &journal, vec![]);
    expect_ok(env.send(vec![ed25519_ix(data), bless], &[&payer]));
    assert!(env.is_blessed_as(&journal.signer, ImageKind::Builder));
}

// How checkers are asked -----------------------------------------------------------

/// A checker is handed its accounts read-only whatever the transaction marks
/// them: marking the checker's state writable changes nothing, and the
/// checker cannot write it.
#[test]
fn a_checker_gets_its_accounts_read_only_whatever_the_transaction_marks() {
    let mut env = Env::new();
    let journal = service_journal(&env);
    let attestor = env.attestor.insecure_clone();
    let payer = env.payer.insecure_clone();
    let (seal, ed25519) = env.seal(&attestor, &journal);
    let mut bless = bless_service(&env, &payer.pubkey(), &journal, seal);
    let state_before = env.svm.get_account(&env.checker).unwrap();
    let position = bless
        .accounts
        .iter()
        .position(|meta| meta.pubkey == env.checker)
        .unwrap();
    bless.accounts[position] = AccountMeta::new(env.checker, false);
    expect_ok(env.send(vec![ed25519, bless], &[&payer]));
    assert!(env.is_blessed_as(&journal.signer, ImageKind::Builder));
    assert_eq!(env.svm.get_account(&env.checker).unwrap(), state_before);
}

/// Accepted by design (build plan, "Solana programs"): a checker whose call
/// fails, rather than answering no, fails the whole blessing, even when an
/// older checker would accept the proof; the admin retires it at once and
/// blessings resume. Here the newest checker is given the wrong third
/// account, which the development checker refuses outright.
#[test]
fn a_checker_that_fails_fails_the_blessing_until_it_is_retired() {
    let mut env = Env::new();
    let newest = env.propose_checker(Keypair::new().pubkey());
    env.skip(TIMELOCK);
    let journal = service_journal(&env);
    let attestor = env.attestor.insecure_clone();
    let payer = env.payer.insecure_clone();
    let (seal, ed25519) = env.seal(&attestor, &journal);
    let mut bless = bless_service(&env, &payer.pubkey(), &journal, seal.clone());
    let position = bless
        .accounts
        .iter()
        .position(|meta| meta.pubkey == newest)
        .unwrap();
    // The newest checker's third account is the registry's config, not the
    // instructions sysvar.
    bless.accounts[position + 1] = AccountMeta::new_readonly(registry::config_address(), false);
    let result = env.send(vec![ed25519.clone(), bless], &[&payer]);
    assert!(result.is_err(), "a failing checker was skipped");
    assert_ne!(
        code_of(&result),
        Some(registry_error(RegistryError::ProofRejected)),
        "the failure read as a refusal"
    );
    assert!(!env.is_blessed_as(&journal.signer, ImageKind::Builder));

    expect_ok(env.retire_verifier(newest));
    let bless = bless_service(&env, &payer.pubkey(), &journal, seal);
    expect_ok(env.send(vec![ed25519, bless], &[&payer]));
    assert!(env.is_blessed_as(&journal.signer, ImageKind::Builder));
}

/// The registry does not tie a development checker to its own cluster: a
/// checker created for another cluster, once the admin proposes it, accepts
/// the seal signed for that cluster's digest. The leaf still names this
/// cluster, so the attestor must have meant a key for it; recorded as a
/// note, since the checker is the admin's choice and never serves mainnet.
#[test]
fn a_development_checker_for_another_cluster_is_accepted_once_proposed() {
    let mut env = Env::new();
    let other_key = Keypair::new();
    let seed = keccak(&[b"other cluster"]);
    let state = Pubkey::find_program_address(
        &[
            shonin_dev_verifier::CHECKER_SEED,
            other_key.pubkey().as_ref(),
            &seed,
        ],
        &shonin_dev_verifier::ID,
    )
    .0;
    let payer = env.payer.insecure_clone();
    let create = Instruction {
        program_id: shonin_dev_verifier::ID,
        accounts: anchor_lang::ToAccountMetas::to_account_metas(
            &shonin_dev_verifier::accounts::CreateChecker {
                payer: payer.pubkey(),
                checker: state,
                system_program: solana_sdk_ids::system_program::ID,
            },
            None,
        ),
        data: anchor_lang::InstructionData::data(
            &shonin_dev_verifier::instruction::CreateChecker {
                attestation_key: other_key.pubkey(),
                seed,
                cluster_name: "devnet".to_string(),
            },
        ),
    };
    expect_ok(env.send(vec![create], &[&payer]));
    let ix = env.ix_propose_verifier(&env.admin.pubkey(), shonin_dev_verifier::ID, None, state);
    expect_ok(env.as_admin(ix));
    env.skip(TIMELOCK);

    let journal = service_journal(&env);
    let bytes = borsh::to_vec(&journal).unwrap();
    let devnet = shonin_dev_verifier::attestation_digest(&cluster_value("devnet"), &bytes);
    let bless = bless_service(&env, &payer.pubkey(), &journal, vec![]);
    expect_ok(env.send(vec![Env::ed25519(&other_key, &devnet), bless], &[&payer]));
    assert!(env.is_blessed_as(&journal.signer, ImageKind::Builder));
    assert_eq!(env.key_info(&journal.signer).unwrap().verifier, state);
}
