//! The checks over a genuine Confidential Space token (see
//! [`fixture::REAL_TOKEN`]), so what the guest accepts is known to include
//! what Google really issues, in the shapes it really uses.

use serde_json::Value;

use super::super::*;
use crate::fixture::{self, REAL_EXP, REAL_IAT, real_binding, real_input};

/// The real token's claims, decoded without checking anything.
fn real_claims() -> Value {
    let payload = fixture::REAL_TOKEN.split('.').nth(1).unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap()
}

fn header_of(token: &str) -> Value {
    let header = token.split('.').next().unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).unwrap()).unwrap()
}

#[test]
fn the_real_token_passes_inside_its_hour_with_google_s_published_keys() {
    for now in [REAL_IAT, REAL_IAT + 60, REAL_EXP - 1] {
        let journal = verify(&real_input(now)).unwrap();
        assert_eq!(
            hex::encode(journal.image_digest),
            fixture::REAL_IMAGE_DIGEST.trim_start_matches("sha256:")
        );
        assert_eq!(journal.project_id, project_id(fixture::REAL_PROJECT));
        assert_eq!(
            hex::encode(journal.project_id),
            "b585fbc00011b8d92eaf5863288849faeb95b4ecfb1b28dab38fc94fbd817e1f"
        );
        assert_eq!(journal.vaults_root, real_binding().vaults_root);
        assert_eq!(hex::encode(journal.evm_signer), fixture::REAL_EVM_SIGNER);
        assert_eq!(journal.solana_signer, [0; 32]);
        assert_eq!(journal.issued_at, REAL_IAT);
        assert_eq!(hex::encode(journal.key_hash), fixture::REAL_KEY_HASH);
    }
}

#[test]
fn the_real_token_is_refused_at_its_expiry_and_before_its_issue() {
    assert_eq!(verify(&real_input(REAL_EXP)), Err(Error::NotFresh));
    assert_eq!(verify(&real_input(REAL_EXP + 1)), Err(Error::NotFresh));
    assert_eq!(verify(&real_input(REAL_IAT - 1)), Err(Error::NotFresh));
}

#[test]
fn the_real_token_s_times_are_the_fixture_s() {
    let claims = real_claims();
    assert_eq!(claims["iat"].as_u64(), Some(REAL_IAT));
    assert_eq!(claims["nbf"].as_u64(), Some(REAL_IAT));
    assert_eq!(claims["exp"].as_u64(), Some(REAL_EXP));
}

#[test]
fn the_real_token_s_nonce_is_one_string_and_binds_the_fixture_s_keys_and_root() {
    let claims = real_claims();
    // One nonce comes back as a plain string, not a one-element array.
    assert_eq!(
        claims["eat_nonce"].as_str(),
        Some(real_binding().nonce().as_str())
    );
    // The binding file written beside the token says the same.
    let file: Value =
        serde_json::from_str(include_str!("../../testdata/cs-token-2026-10-06.binding.json"))
            .unwrap();
    assert_eq!(file["nonce"].as_str(), Some(real_binding().nonce().as_str()));
    assert_eq!(
        file["vaults_root"].as_str(),
        Some(format!("0x{}", hex::encode(real_binding().vaults_root)).as_str())
    );
}

#[test]
fn the_real_token_carries_no_override_and_a_tdx_object() {
    let claims = real_claims();
    // Nothing overridden: the claims are absent, not empty.
    assert!(claims.pointer("/submods/container/cmd_override").is_none());
    assert!(claims.pointer("/submods/container/env_override").is_none());
    // `tdx` is an object (the documentation shows an array); nothing checked
    // depends on its shape.
    assert!(claims["tdx"].is_object());
    assert_eq!(claims["hwmodel"].as_str(), Some(HARDWARE));
    assert_eq!(claims["swname"].as_str(), Some(SOFTWARE));
    assert_eq!(claims["dbgstat"].as_str(), Some(DEBUG_DISABLED));
    assert_eq!(claims["aud"].as_str(), Some(AUDIENCE));
    assert_eq!(claims["iss"].as_str(), Some(ISSUER));
}

#[test]
fn the_real_token_s_signature_covers_its_claims() {
    let mut input = real_input(REAL_IAT + 60);
    let mut claims = real_claims();
    claims["submods"]["gce"]["project_id"] = Value::from("another-project");
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let parts: Vec<&str> = fixture::REAL_TOKEN.split('.').collect();
    input.token = format!("{}.{}.{}", parts[0], payload, parts[2]);
    assert_eq!(verify(&input), Err(Error::BadSignature));
}

#[test]
fn the_real_token_is_refused_by_a_check_given_none_of_google_s_keys() {
    let mut input = real_input(REAL_IAT + 60);
    input.keys = vec![fixture::test_jwk()];
    assert_eq!(verify(&input), Err(Error::UnknownKey));
}

#[test]
fn the_real_pki_token_is_refused_for_want_of_its_key() {
    let header = header_of(fixture::REAL_PKI_TOKEN);
    assert!(header["x5c"].is_array());
    let mut input = real_input(REAL_IAT + 60);
    input.token = fixture::REAL_PKI_TOKEN.into();
    assert_eq!(verify(&input), Err(Error::UnknownKey));
}

#[test]
fn the_real_token_is_refused_for_another_binding() {
    let other_root = Binding {
        vaults_root: [0x22; 32],
        ..real_binding()
    };
    let other_key = Binding {
        evm_signer: [0x33; 20],
        ..real_binding()
    };
    let with_solana = Binding {
        solana_signer: [0x44; 32],
        ..real_binding()
    };
    for binding in [other_root, other_key, with_solana] {
        let mut input = real_input(REAL_IAT + 60);
        input.binding = binding;
        assert_eq!(verify(&input), Err(Error::Nonce));
    }
}
