//! A Confidential Space checker that pins Google's Confidential Space Root
//! CA: a real PKI token's proof, made for the EVM checker, is accepted by the
//! Solana checker for the guest and the root it pins, and by nothing that
//! trusts only Google's signing keys or another root. Such a checker is a
//! new one beside the earlier one, which keeps accepting what the earlier
//! guest proved until it is retired.

use anchor_lang::prelude::Pubkey;
use shonin_registry::error::RegistryError;
use shonin_solana_tests::real_pki_token as pki;
use shonin_solana_tests::real_token as real;
use shonin_solana_tests::*;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// The platform with a checker for `image` trusting `trusted` allowed
/// beside the development checker, the real tokens' image and project
/// allowed, and the clock just after the PKI token was issued.
fn with_checker(image: [u8; 32], trusted: &[[u8; 32]]) -> (Env, Pubkey) {
    let mut env = Env::new();
    let checker = env.create_cs_checker(image, trusted);
    expect_ok(env.propose_cs_checker(checker));
    let admin = env.admin.insecure_clone();
    let ixs = vec![
        env.ix_propose_image(&admin.pubkey(), real::image_digest(), ImageKind::Executor),
        env.ix_propose_project(&admin.pubkey(), real::project_id()),
    ];
    expect_ok(env.send(ixs, &[&admin]));
    env.skip(TIMELOCK);
    env.warp(pki::ISSUED_AT + MINUTE);
    (env, checker)
}

/// Google's root and Google's signing keys as pinned: the checker a
/// rotation creates.
fn root_and_keys() -> Vec<[u8; 32]> {
    let mut trusted = vec![pki::root_hash()];
    trusted.extend(real::google_keys());
    trusted
}

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

#[test]
fn a_pki_token_s_proof_passes_a_checker_pinning_google_s_root() {
    for trusted in [vec![pki::root_hash()], root_and_keys()] {
        let (mut env, _) = with_checker(pki::image_id(), &trusted);
        // The checker accepts it, so the registry goes on to its own rules:
        // the instance served no Solana cluster, so its journal names no
        // Solana key (anything the checker refused would be no proof).
        expect_code(
            bless(&mut env, &pki::journal(), pki::seal()),
            registry_error(RegistryError::NoSigner),
        );
    }
}

#[test]
fn a_checker_trusting_only_google_s_signing_keys_or_another_root_refuses_a_pki_token() {
    for trusted in [real::google_keys(), vec![hash("another root")]] {
        let (mut env, _) = with_checker(pki::image_id(), &trusted);
        expect_code(
            bless(&mut env, &pki::journal(), pki::seal()),
            registry_error(RegistryError::ProofRejected),
        );
    }
}

#[test]
fn a_pki_token_s_proof_naming_a_trusted_signing_key_instead_of_the_root_is_no_proof() {
    let (mut env, _) = with_checker(pki::image_id(), &root_and_keys());
    let mut seal = pki::seal();
    let at = seal.len() - 32;
    seal[at..].copy_from_slice(&real::key_hash());
    expect_code(
        bless(&mut env, &pki::journal(), seal),
        registry_error(RegistryError::ProofRejected),
    );
}

#[test]
fn the_new_checker_has_its_own_address_beside_the_earlier_one() {
    let mut env = Env::new();
    let earlier = env.create_cs_checker(real::image_id(), &real::google_keys());
    let new = env.create_cs_checker(pki::image_id(), &root_and_keys());
    assert_ne!(earlier, new);
    expect_ok(env.propose_cs_checker(earlier));
    expect_ok(env.propose_cs_checker(new));
    env.skip(TIMELOCK);
    assert!(env.is_verifier_allowed(&earlier));
    assert!(env.is_verifier_allowed(&new));
}

#[test]
fn during_the_overlap_each_guest_s_proof_passes_only_its_own_checker() {
    // The earlier guest's proof of the OIDC token passes the earlier
    // checker, and not the new one though it trusts the same keys; the new
    // guest's proof of the PKI token passes the new one only.
    let cases = [
        (real::image_id(), real::google_keys(), true, false),
        (pki::image_id(), root_and_keys(), false, true),
    ];
    for (image, trusted, earlier_passes, pki_passes) in cases {
        let (mut env, _) = with_checker(image, &trusted);
        env.warp(real::ISSUED_AT + MINUTE);
        let earlier = bless(&mut env, &real::journal(), real::seal());
        env.warp(pki::ISSUED_AT + MINUTE);
        let new = bless(&mut env, &pki::journal(), pki::seal());
        for (result, passes) in [(earlier, earlier_passes), (new, pki_passes)] {
            let code = if passes {
                RegistryError::NoSigner
            } else {
                RegistryError::ProofRejected
            };
            expect_code(result, registry_error(code));
        }
    }
}
