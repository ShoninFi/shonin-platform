use serde_json::{Value, json};

use super::*;
use crate::fixture::{self, binding, claims, header, input, sign, sign_with, test_private_key};

const IAT: u64 = 1_790_000_000;
const NOW: u64 = IAT + 60;

fn passes(claims: &Value) -> Result<Journal, Error> {
    let mut input = input(&binding(), IAT, NOW);
    input.token = sign(claims);
    verify(&input)
}

fn with(mut claims: Value, pointer: &str, value: Value) -> Value {
    *claims.pointer_mut(pointer).expect("the claim exists") = value;
    claims
}

fn without(mut claims: Value, parent: &str, field: &str) -> Value {
    claims
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .expect("the parent exists")
        .remove(field);
    claims
}

#[test]
fn a_production_token_yields_the_journal_of_its_image_project_keys_and_time() {
    let journal = verify(&input(&binding(), IAT, NOW)).unwrap();
    assert_eq!(
        hex::encode(journal.image_digest),
        fixture::IMAGE_DIGEST.trim_start_matches("sha256:")
    );
    assert_eq!(journal.project_id, project_id(fixture::PROJECT));
    assert_eq!(journal.vaults_root, binding().vaults_root);
    assert_eq!(journal.evm_signer, binding().evm_signer);
    assert_eq!(journal.solana_signer, binding().solana_signer);
    assert_eq!(journal.issued_at, IAT);
    assert_eq!(journal.key_hash, fixture::test_jwk().key_hash());
}

#[test]
fn a_token_signed_by_a_key_not_given_is_refused() {
    let mut input = input(&binding(), IAT, NOW);
    input.keys.retain(|key| key.kid != fixture::TEST_KID);
    assert_eq!(verify(&input), Err(Error::UnknownKey));
}

#[test]
fn a_token_whose_signature_was_made_by_another_key_under_the_same_kid_is_refused() {
    let other = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let mut input = input(&binding(), IAT, NOW);
    input.token = sign_with(&other, &header(fixture::TEST_KID), &claims(&binding(), IAT));
    assert_eq!(verify(&input), Err(Error::BadSignature));
}

#[test]
fn a_claim_changed_after_signing_breaks_the_signature() {
    let mut input = input(&binding(), IAT, NOW);
    let parts: Vec<&str> = input.token.split('.').collect();
    let forged = with(
        claims(&binding(), IAT),
        "/dbgstat",
        json!(DEBUG_DISABLED.to_uppercase()),
    );
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap());
    input.token = format!("{}.{}.{}", parts[0], payload, parts[2]);
    assert_eq!(verify(&input), Err(Error::BadSignature));
}

#[test]
fn only_rs256_is_accepted() {
    for alg in ["none", "HS256", "RS512", "PS256", "ES256"] {
        let mut input = input(&binding(), IAT, NOW);
        let header = json!({"alg": alg, "kid": fixture::TEST_KID});
        input.token = sign_with(&test_private_key(), &header, &claims(&binding(), IAT));
        assert_eq!(verify(&input), Err(Error::Algorithm), "{alg}");
    }
}

#[test]
fn a_key_smaller_than_2048_bits_is_refused() {
    let small = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap();
    let public = rsa::RsaPublicKey::from(&small);
    let mut input = input(&binding(), IAT, NOW);
    input.keys = vec![Jwk {
        kid: "small".into(),
        n: rsa::traits::PublicKeyParts::n(&public).to_bytes_be(),
        e: rsa::traits::PublicKeyParts::e(&public).to_bytes_be(),
    }];
    input.token = sign_with(&small, &header("small"), &claims(&binding(), IAT));
    assert_eq!(verify(&input), Err(Error::WeakKey));
}

#[test]
fn a_debug_image_is_refused() {
    let claims = with(claims(&binding(), IAT), "/dbgstat", json!("enabled"));
    assert_eq!(passes(&claims), Err(Error::Claim("/dbgstat")));
}

#[test]
fn only_the_confidential_space_launcher_is_accepted() {
    let claims = with(claims(&binding(), IAT), "/swname", json!("GCE"));
    assert_eq!(passes(&claims), Err(Error::Claim("/swname")));
}

#[test]
fn only_intel_tdx_hardware_is_accepted() {
    for model in ["GCP_SHIELDED_VM", "GCP_AMD_SEV", "GCP_AMD_SEV_ES"] {
        let claims = with(claims(&binding(), IAT), "/hwmodel", json!(model));
        assert_eq!(passes(&claims), Err(Error::Claim("/hwmodel")), "{model}");
    }
}

#[test]
fn secure_boot_must_be_on() {
    let claims = with(claims(&binding(), IAT), "/secboot", json!(false));
    assert_eq!(passes(&claims), Err(Error::Claim("/secboot")));
}

#[test]
fn a_launcher_version_google_no_longer_marks_stable_is_refused() {
    let claims = with(
        claims(&binding(), IAT),
        "/submods/confidential_space/support_attributes",
        json!(["EXPERIMENTAL"]),
    );
    assert_eq!(
        passes(&claims),
        Err(Error::Claim(
            "/submods/confidential_space/support_attributes"
        ))
    );
}

#[test]
fn another_issuer_is_refused() {
    let claims = with(
        claims(&binding(), IAT),
        "/iss",
        json!("https://accounts.google.com"),
    );
    assert_eq!(passes(&claims), Err(Error::Claim("/iss")));
}

#[test]
fn a_token_for_another_audience_or_for_several_is_refused() {
    for aud in [
        json!("https://sts.googleapis.com"),
        json!([AUDIENCE, "https://example.com"]),
        json!([]),
    ] {
        let claims = with(claims(&binding(), IAT), "/aud", aud.clone());
        assert_eq!(passes(&claims), Err(Error::Claim("/aud")), "{aud}");
    }
    let one = with(claims(&binding(), IAT), "/aud", json!([AUDIENCE]));
    assert!(passes(&one).is_ok());
}

#[test]
fn a_token_is_accepted_only_from_its_issue_until_its_expiry() {
    let at = |now| {
        let mut input = input(&binding(), IAT, now);
        input.now = now;
        verify(&input).map(|_| ())
    };
    assert_eq!(at(IAT - 1), Err(Error::NotFresh));
    assert_eq!(at(IAT), Ok(()));
    assert_eq!(at(IAT + 3599), Ok(()));
    assert_eq!(at(IAT + 3600), Err(Error::NotFresh));
}

#[test]
fn a_token_not_yet_valid_is_refused() {
    let claims = with(claims(&binding(), IAT), "/nbf", json!(NOW + 1));
    assert_eq!(passes(&claims), Err(Error::NotFresh));
}

#[test]
fn an_operator_override_of_the_image_s_arguments_or_environment_is_refused() {
    let base = claims(&binding(), IAT);
    let cmd = with(base.clone(), "/submods/container", {
        let mut container = base.pointer("/submods/container").unwrap().clone();
        container["cmd_override"] = json!(["--unsafe"]);
        container
    });
    assert_eq!(
        passes(&cmd),
        Err(Error::Claim("/submods/container/cmd_override"))
    );
    let env = with(base.clone(), "/submods/container", {
        let mut container = base.pointer("/submods/container").unwrap().clone();
        container["env_override"] = json!({"RUST_LOG": "trace"});
        container
    });
    assert_eq!(
        passes(&env),
        Err(Error::Claim("/submods/container/env_override"))
    );
    // Empty overrides override nothing.
    let empty = with(base.clone(), "/submods/container", {
        let mut container = base.pointer("/submods/container").unwrap().clone();
        container["cmd_override"] = json!([]);
        container["env_override"] = json!({});
        container
    });
    assert!(passes(&empty).is_ok());
}

#[test]
fn an_image_digest_that_is_not_a_sha256_digest_is_refused() {
    for digest in [
        "sha256:3b9e",
        "sha512:3b9e5f2a6c1d4e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7",
        "3b9e5f2a6c1d4e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7",
        "sha256:zz9e5f2a6c1d4e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7",
    ] {
        let claims = with(
            claims(&binding(), IAT),
            "/submods/container/image_digest",
            json!(digest),
        );
        assert_eq!(
            passes(&claims),
            Err(Error::Claim("/submods/container/image_digest")),
            "{digest}"
        );
    }
}

#[test]
fn a_token_with_no_project_is_refused() {
    let claims = without(claims(&binding(), IAT), "/submods/gce", "project_id");
    assert_eq!(
        passes(&claims),
        Err(Error::Claim("/submods/gce/project_id"))
    );
}

#[test]
fn a_token_whose_nonce_binds_other_keys_or_another_root_is_refused() {
    let theirs = Binding {
        evm_signer: [0xBA; 20],
        ..binding()
    };
    let mut theirs_input = input(&binding(), IAT, NOW);
    theirs_input.token = sign(&claims(&theirs, IAT));
    assert_eq!(verify(&theirs_input), Err(Error::Nonce));

    for changed in [
        Binding {
            solana_signer: [1; 32],
            ..binding()
        },
        Binding {
            vaults_root: [2; 32],
            ..binding()
        },
    ] {
        let mut input = input(&binding(), IAT, NOW);
        input.binding = changed;
        assert_eq!(verify(&input), Err(Error::Nonce));
    }
}

#[test]
fn the_nonce_may_be_one_of_several() {
    let claims = with(
        claims(&binding(), IAT),
        "/eat_nonce",
        json!(["an-unrelated-nonce", binding().nonce()]),
    );
    assert!(passes(&claims).is_ok());
    let single = with(claims.clone(), "/eat_nonce", json!(binding().nonce()));
    assert!(passes(&single).is_ok());
}

#[test]
fn a_token_that_is_not_three_parts_is_refused() {
    let mut input = input(&binding(), IAT, NOW);
    input.token.push_str(".extra");
    assert_eq!(verify(&input), Err(Error::Malformed("structure")));
}

#[test]
fn the_pinned_key_set_is_one_of_the_dated_sets_the_scheduled_check_reads() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata");
    let dated: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with("google-jwks-") && name.ends_with(".json"))
        .map(|name| std::fs::read_to_string(format!("{dir}/{name}")).unwrap())
        .collect();
    assert!(dated.iter().any(|set| set == PINNED_GOOGLE_JWKS));
    assert!(!Jwk::from_jwks(PINNED_GOOGLE_JWKS).unwrap().is_empty());
}

#[test]
fn google_s_published_keys_parse_and_hash_by_their_material_alone() {
    let keys = Jwk::from_jwks(fixture::GOOGLE_JWKS_2026_10_06).unwrap();
    assert_eq!(keys.len(), 2);
    for key in &keys {
        assert_eq!(key.n.len(), 256, "2048-bit keys");
        assert_eq!(key.e, vec![1, 0, 1]);
    }
    assert_ne!(keys[0].key_hash(), keys[1].key_hash());
    let relabelled = Jwk {
        kid: "another".into(),
        ..keys[0].clone()
    };
    assert_eq!(relabelled.key_hash(), keys[0].key_hash());
    let padded = Jwk {
        n: [&[0u8][..], &keys[0].n].concat(),
        ..keys[0].clone()
    };
    assert_eq!(padded.key_hash(), keys[0].key_hash());
}

#[test]
fn the_canonical_journal_round_trips_and_is_188_bytes() {
    let journal = verify(&input(&binding(), IAT, NOW)).unwrap();
    let bytes = journal.encode();
    assert_eq!(bytes.len(), 188);
    assert_eq!(Journal::decode(&bytes), Some(journal));
    assert_eq!(Journal::decode(&bytes[..187]), None);
}

#[test]
fn the_evm_journal_is_the_registry_s_abi_encoding() {
    let journal = verify(&input(&binding(), IAT, NOW)).unwrap();
    let evm = journal.evm();
    assert_eq!(&evm[0..32], &journal.image_digest);
    assert_eq!(&evm[32..64], &journal.project_id);
    assert_eq!(&evm[64..76], &[0u8; 12]);
    assert_eq!(&evm[76..96], &journal.evm_signer);
    assert_eq!(&evm[96..128], &journal.vaults_root);
    assert_eq!(&evm[128..152], &[0u8; 24]);
    assert_eq!(u64::from_be_bytes(evm[152..160].try_into().unwrap()), IAT);
}

#[test]
fn the_solana_journal_is_the_registry_s_borsh_encoding() {
    let journal = verify(&input(&binding(), IAT, NOW)).unwrap();
    let solana = journal.solana();
    assert_eq!(&solana[64..96], &journal.solana_signer);
    assert_eq!(&solana[96..128], &journal.vaults_root);
    assert_eq!(
        u64::from_le_bytes(solana[128..136].try_into().unwrap()),
        IAT
    );
}

#[test]
fn the_nonce_is_64_hex_characters_within_confidential_space_s_limits() {
    let nonce = binding().nonce();
    assert_eq!(nonce.len(), 64);
    assert!((8..=88).contains(&nonce.len()));
    assert!(nonce.bytes().all(|b| b.is_ascii_hexdigit()));
}

#[test]
fn each_chain_rebuilds_the_canonical_journal_from_its_own_journal_and_the_seal_s_tail() {
    let journal = verify(&input(&binding(), IAT, NOW)).unwrap();
    assert_eq!(
        Journal::from_evm(&journal.evm(), &journal.evm_seal_tail()),
        Some(journal)
    );
    assert_eq!(
        Journal::from_solana(&journal.solana(), &journal.solana_seal_tail()),
        Some(journal)
    );
    // An EVM journal with stray bytes where abi.encode puts zeros is not one
    // the registry could have decoded the same way.
    let mut dirty = journal.evm();
    dirty[64] = 1;
    assert_eq!(Journal::from_evm(&dirty, &journal.evm_seal_tail()), None);
    assert_eq!(Journal::from_evm(&journal.evm(), &[0; 63]), None);
}

mod pki;
mod real_token;
