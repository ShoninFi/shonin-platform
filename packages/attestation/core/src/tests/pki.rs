//! The PKI path over a chain of the tests' own (see [`fixture::chain`]):
//! what a chain must be for its token to pass, and that every claim check
//! applies to a PKI token as to an OIDC one.

use serde_json::{Value, json};

use super::super::*;
use crate::fixture::chain::{
    self, INTERMEDIATE_SEED, LEAF_SEED, OTHER_ROOT_SEED, ROOT_SEED, Spec, key,
};
use crate::fixture::{self, binding, claims};

const IAT: u64 = 1_790_000_000;
const NOW: u64 = IAT + 60;

/// The token of `claims` under the chain the specs make, signed by the
/// leaf's key, checked at `NOW`.
fn with_chain(leaf: &Spec, intermediate: &Spec, root: &Spec) -> Result<Journal, Error> {
    let chain = chain::chain_of(leaf, intermediate, root);
    verify(&Input {
        token: chain::token(&chain, &key(LEAF_SEED), &claims(&binding(), IAT)),
        keys: Vec::new(),
        now: NOW,
        binding: binding(),
    })
}

fn leaf() -> Spec {
    Spec::leaf(IAT)
}

fn intermediate() -> Spec {
    Spec::intermediate(IAT)
}

fn root() -> Spec {
    Spec::root(IAT)
}

fn signed(chain: &[Vec<u8>], signer: &rsa::RsaPrivateKey) -> Result<Journal, Error> {
    verify(&Input {
        token: chain::token(chain, signer, &claims(&binding(), IAT)),
        keys: Vec::new(),
        now: NOW,
        binding: binding(),
    })
}

#[test]
fn a_pki_token_chained_to_its_root_names_the_root_s_hash_in_the_journal() {
    let journal = verify(&chain::input(&binding(), IAT, NOW)).unwrap();
    assert_eq!(journal.key_hash, chain::root_hash(IAT));
    assert_eq!(
        journal.key_hash,
        pki::root_hash(chain::chain(IAT).last().unwrap())
    );
    // Everything else is what an OIDC token with the same claims says.
    let oidc = verify(&fixture::input(&binding(), IAT, NOW)).unwrap();
    assert_eq!(
        Journal {
            key_hash: oidc.key_hash,
            ..journal
        },
        oidc
    );
}

#[test]
fn the_keys_given_play_no_part_for_a_pki_token() {
    let mut input = chain::input(&binding(), IAT, NOW);
    input.keys = vec![fixture::test_jwk()];
    assert_eq!(verify(&input).unwrap().key_hash, chain::root_hash(IAT));
}

#[test]
fn a_chain_ending_in_another_root_names_that_root_not_ours() {
    // A self-consistent chain under a root no checker pins passes the guest,
    // and names its own root: the checker's pin is what refuses it.
    let other = key(OTHER_ROOT_SEED);
    let intermediate_key = key(INTERMEDIATE_SEED);
    let chain = vec![
        chain::certificate(&leaf(), &key(LEAF_SEED).to_public_key(), &intermediate_key),
        chain::certificate(&intermediate(), &intermediate_key.to_public_key(), &other),
        chain::certificate(&root(), &other.to_public_key(), &other),
    ];
    let journal = signed(&chain, &key(LEAF_SEED)).unwrap();
    assert_eq!(journal.key_hash, pki::root_hash(&chain[2]));
    assert_ne!(journal.key_hash, chain::root_hash(IAT));
    assert_ne!(journal.key_hash, confidential_space_root_hash());
}

#[test]
fn an_intermediate_not_signed_by_the_root_presented_is_refused() {
    // Our root presented, the intermediate signed by another key.
    let intermediate_key = key(INTERMEDIATE_SEED);
    let chain = vec![
        chain::certificate(&leaf(), &key(LEAF_SEED).to_public_key(), &intermediate_key),
        chain::certificate(
            &intermediate(),
            &intermediate_key.to_public_key(),
            &key(OTHER_ROOT_SEED),
        ),
        chain::certificate(&root(), &key(ROOT_SEED).to_public_key(), &key(ROOT_SEED)),
    ];
    assert_eq!(
        signed(&chain, &key(LEAF_SEED)),
        Err(Error::Chain("signature"))
    );
}

#[test]
fn a_leaf_not_signed_by_the_intermediate_is_refused() {
    let mut chain = chain::chain(IAT);
    // The leaf signed by the root's key instead.
    chain[0] = chain::certificate(&leaf(), &key(LEAF_SEED).to_public_key(), &key(ROOT_SEED));
    assert_eq!(
        signed(&chain, &key(LEAF_SEED)),
        Err(Error::Chain("signature"))
    );
}

#[test]
fn a_leaf_naming_another_issuer_is_refused() {
    let named = Spec {
        issuer: chain::ROOT_NAME,
        ..leaf()
    };
    assert_eq!(
        with_chain(&named, &intermediate(), &root()),
        Err(Error::Chain("issuer"))
    );
}

#[test]
fn a_token_signed_by_a_key_other_than_the_leaf_s_is_refused() {
    let chain = chain::chain(IAT);
    for other in [
        key(INTERMEDIATE_SEED),
        key(ROOT_SEED),
        fixture::test_private_key(),
    ] {
        assert_eq!(signed(&chain, &other), Err(Error::BadSignature));
    }
}

#[test]
fn a_jwks_key_cannot_vouch_for_a_token_that_carries_a_chain() {
    // The header names the test JWK's kid and carries the chain; the test
    // JWK signed it, and is given. The chain decides: refused.
    let mut header = chain::header(&chain::chain(IAT));
    header["kid"] = json!(fixture::TEST_KID);
    let mut input = fixture::input(&binding(), IAT, NOW);
    input.token = fixture::sign_with(
        &fixture::test_private_key(),
        &header,
        &claims(&binding(), IAT),
    );
    assert_eq!(verify(&input), Err(Error::BadSignature));
}

#[test]
fn a_claim_changed_after_signing_breaks_a_pki_token_s_signature() {
    let mut input = chain::input(&binding(), IAT, NOW);
    let parts: Vec<&str> = input.token.split('.').collect();
    let mut forged = claims(&binding(), IAT);
    forged["submods"]["gce"]["project_id"] = json!("another-project");
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap());
    input.token = format!("{}.{}.{}", parts[0], payload, parts[2]);
    assert_eq!(verify(&input), Err(Error::BadSignature));
}

#[test]
fn a_certificate_expired_when_the_token_was_issued_is_refused() {
    let expired = |spec: Spec| Spec {
        not_after: IAT - 1,
        ..spec
    };
    for (leaf, intermediate, root) in [
        (expired(leaf()), intermediate(), root()),
        (leaf(), expired(intermediate()), root()),
        (leaf(), intermediate(), expired(root())),
    ] {
        assert_eq!(
            with_chain(&leaf, &intermediate, &root),
            Err(Error::Chain("validity"))
        );
    }
}

#[test]
fn a_certificate_not_yet_valid_when_the_token_was_issued_is_refused() {
    let early = |spec: Spec| Spec {
        not_before: IAT + 1,
        ..spec
    };
    for (leaf, intermediate, root) in [
        (early(leaf()), intermediate(), root()),
        (leaf(), early(intermediate()), root()),
        (leaf(), intermediate(), early(root())),
    ] {
        assert_eq!(
            with_chain(&leaf, &intermediate, &root),
            Err(Error::Chain("validity"))
        );
    }
}

#[test]
fn validity_is_judged_at_the_token_s_issue_inclusive_at_both_ends() {
    // Valid from exactly the issue, until exactly the issue.
    let exact = Spec {
        not_before: IAT,
        not_after: IAT,
        ..leaf()
    };
    assert!(with_chain(&exact, &intermediate(), &root()).is_ok());
    // The leaf expires after the issue but before the prover's clock: the
    // token was valid when issued, and is judged by that.
    let lapsing = Spec {
        not_after: IAT + 30,
        ..leaf()
    };
    assert!(with_chain(&lapsing, &intermediate(), &root()).is_ok());
}

#[test]
fn an_intermediate_without_the_ca_flag_is_refused() {
    for basic in [Some((false, None)), None] {
        let not_ca = Spec {
            basic,
            ..intermediate()
        };
        assert_eq!(
            with_chain(&leaf(), &not_ca, &root()),
            Err(Error::Chain("not a CA"))
        );
    }
}

#[test]
fn a_root_without_the_ca_flag_is_refused() {
    let not_ca = Spec {
        basic: Some((false, None)),
        ..root()
    };
    assert_eq!(
        with_chain(&leaf(), &intermediate(), &not_ca),
        Err(Error::Chain("not a CA"))
    );
}

#[test]
fn a_ca_whose_key_may_not_sign_certificates_is_refused() {
    use x509_cert::ext::pkix::KeyUsages;
    for usage in [None, Some(KeyUsages::DigitalSignature | KeyUsages::CRLSign)] {
        let cannot_sign = Spec {
            usage,
            ..intermediate()
        };
        assert_eq!(
            with_chain(&leaf(), &cannot_sign, &root()),
            Err(Error::Chain("key usage"))
        );
    }
}

#[test]
fn a_leaf_that_is_a_ca_is_refused() {
    let ca = Spec {
        basic: Some((true, None)),
        ..leaf()
    };
    assert_eq!(
        with_chain(&ca, &intermediate(), &root()),
        Err(Error::Chain("leaf is a CA"))
    );
}

#[test]
fn a_leaf_that_says_its_key_is_not_for_signing_is_refused() {
    use x509_cert::ext::pkix::KeyUsages;
    let encipher = Spec {
        usage: Some(KeyUsages::KeyEncipherment.into()),
        ..leaf()
    };
    assert_eq!(
        with_chain(&encipher, &intermediate(), &root()),
        Err(Error::Chain("key usage"))
    );
    let signing = Spec {
        usage: Some(KeyUsages::DigitalSignature.into()),
        ..leaf()
    };
    assert!(with_chain(&signing, &intermediate(), &root()).is_ok());
    // No basic constraints at all on the leaf is not a CA either.
    let bare = Spec {
        basic: None,
        ..leaf()
    };
    assert!(with_chain(&bare, &intermediate(), &root()).is_ok());
}

#[test]
fn a_path_length_constraint_is_kept() {
    // The root allows no CA below it but the intermediate: none.
    let strict = Spec {
        basic: Some((true, Some(0))),
        ..root()
    };
    assert_eq!(
        with_chain(&leaf(), &intermediate(), &strict),
        Err(Error::Chain("path length"))
    );
    let one = Spec {
        basic: Some((true, Some(1))),
        ..root()
    };
    assert!(with_chain(&leaf(), &intermediate(), &one).is_ok());
    // An intermediate allowing no CA below it, with none below it.
    let last = Spec {
        basic: Some((true, Some(0))),
        ..intermediate()
    };
    assert!(with_chain(&leaf(), &last, &root()).is_ok());
}

#[test]
fn an_unknown_critical_extension_is_refused() {
    for (leaf, intermediate) in [
        (
            Spec {
                unknown_critical: true,
                ..leaf()
            },
            intermediate(),
        ),
        (
            leaf(),
            Spec {
                unknown_critical: true,
                ..intermediate()
            },
        ),
    ] {
        assert_eq!(
            with_chain(&leaf, &intermediate, &root()),
            Err(Error::Chain("critical extension"))
        );
    }
}

#[test]
fn only_sha256_with_rsa_certificates_are_accepted() {
    use x509_cert::der::asn1::ObjectIdentifier;
    // sha1WithRSAEncryption and sha512WithRSAEncryption.
    for oid in ["1.2.840.113549.1.1.5", "1.2.840.113549.1.1.13"] {
        let other = Spec {
            algorithm: ObjectIdentifier::new_unwrap(oid),
            ..leaf()
        };
        assert_eq!(
            with_chain(&other, &intermediate(), &root()),
            Err(Error::Chain("algorithm")),
            "{oid}"
        );
    }
}

#[test]
fn a_ca_key_smaller_than_2048_bits_is_refused() {
    let small = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap();
    let chain = vec![
        chain::certificate(&leaf(), &key(LEAF_SEED).to_public_key(), &small),
        chain::certificate(&intermediate(), &small.to_public_key(), &key(ROOT_SEED)),
        chain::certificate(&root(), &key(ROOT_SEED).to_public_key(), &key(ROOT_SEED)),
    ];
    assert_eq!(signed(&chain, &key(LEAF_SEED)), Err(Error::WeakKey));
}

#[test]
fn a_chain_of_the_wrong_length_or_encoding_is_refused() {
    let full = chain::chain(IAT);
    let five = [full.clone(), full[1..].to_vec()].concat();
    for chain in [full[..1].to_vec(), five] {
        assert_eq!(
            signed(&chain, &key(LEAF_SEED)),
            Err(Error::Chain("x5c")),
            "{} certificates",
            chain.len()
        );
    }
    let token = |x5c: Value| {
        let mut header = chain::header(&full);
        header["x5c"] = x5c;
        verify(&Input {
            token: fixture::sign_with(&key(LEAF_SEED), &header, &claims(&binding(), IAT)),
            keys: Vec::new(),
            now: NOW,
            binding: binding(),
        })
    };
    assert_eq!(token(json!("not a list")), Err(Error::Chain("x5c")));
    assert_eq!(
        token(json!(["%%%", "%%%", "%%%"])),
        Err(Error::Chain("x5c"))
    );
    assert_eq!(
        token(json!(["AAAA", "AAAA", "AAAA"])),
        Err(Error::Chain("certificate"))
    );
}

#[test]
fn a_two_certificate_chain_straight_under_the_root_passes() {
    let direct = Spec {
        issuer: chain::ROOT_NAME,
        ..leaf()
    };
    let chain = vec![
        chain::certificate(&direct, &key(LEAF_SEED).to_public_key(), &key(ROOT_SEED)),
        chain::certificate(&root(), &key(ROOT_SEED).to_public_key(), &key(ROOT_SEED)),
    ];
    let journal = signed(&chain, &key(LEAF_SEED)).unwrap();
    assert_eq!(journal.key_hash, chain::root_hash(IAT));
}

/// Every claim refusal of the OIDC path, on a PKI token: the claims are
/// checked the same, whichever way the token is signed.
#[test]
fn every_claim_check_applies_to_a_pki_token() {
    let base = claims(&binding(), IAT);
    let changed = |pointer: &str, value: Value| {
        let mut claims = base.clone();
        *claims.pointer_mut(pointer).unwrap() = value;
        claims
    };
    let container = |field: &str, value: Value| {
        let mut claims = base.clone();
        claims["submods"]["container"][field] = value;
        claims
    };
    let mut no_project = base.clone();
    no_project["submods"]["gce"]
        .as_object_mut()
        .unwrap()
        .remove("project_id");
    let refusals: Vec<(Value, Error)> = vec![
        (
            changed("/iss", json!("https://accounts.google.com")),
            Error::Claim("/iss"),
        ),
        (
            changed("/aud", json!("https://sts.googleapis.com")),
            Error::Claim("/aud"),
        ),
        (
            changed("/aud", json!([AUDIENCE, "https://example.com"])),
            Error::Claim("/aud"),
        ),
        (
            changed("/dbgstat", json!("enabled")),
            Error::Claim("/dbgstat"),
        ),
        (changed("/swname", json!("GCE")), Error::Claim("/swname")),
        (
            changed("/hwmodel", json!("GCP_AMD_SEV")),
            Error::Claim("/hwmodel"),
        ),
        (changed("/secboot", json!(false)), Error::Claim("/secboot")),
        (
            changed(
                "/submods/confidential_space/support_attributes",
                json!(["EXPERIMENTAL"]),
            ),
            Error::Claim("/submods/confidential_space/support_attributes"),
        ),
        (
            container("cmd_override", json!(["--unsafe"])),
            Error::Claim("/submods/container/cmd_override"),
        ),
        (
            container("env_override", json!({"RUST_LOG": "trace"})),
            Error::Claim("/submods/container/env_override"),
        ),
        (
            changed("/submods/container/image_digest", json!("sha256:3b9e")),
            Error::Claim("/submods/container/image_digest"),
        ),
        (no_project, Error::Claim("/submods/gce/project_id")),
        (changed("/nbf", json!(NOW + 1)), Error::NotFresh),
        (changed("/exp", json!(NOW)), Error::NotFresh),
        (changed("/iat", json!(NOW + 1)), Error::NotFresh),
        (changed("/eat_nonce", json!(["another"])), Error::Nonce),
    ];
    let chain = chain::chain(IAT);
    for (claims, refusal) in refusals {
        let input = Input {
            token: chain::token(&chain, &key(LEAF_SEED), &claims),
            keys: Vec::new(),
            now: NOW,
            binding: binding(),
        };
        assert_eq!(verify(&input), Err(refusal.clone()), "{refusal:?}");
    }
    // And only RS256.
    let mut header = chain::header(&chain);
    header["alg"] = json!("RS512");
    let input = Input {
        token: fixture::sign_with(&key(LEAF_SEED), &header, &base),
        keys: Vec::new(),
        now: NOW,
        binding: binding(),
    };
    assert_eq!(verify(&input), Err(Error::Algorithm));
}

#[test]
fn a_pki_token_is_accepted_only_from_its_issue_until_its_expiry() {
    let at = |now| verify(&chain::input(&binding(), IAT, now)).map(|_| ());
    assert_eq!(at(IAT - 1), Err(Error::NotFresh));
    assert_eq!(at(IAT), Ok(()));
    assert_eq!(at(IAT + 3599), Ok(()));
    assert_eq!(at(IAT + 3600), Err(Error::NotFresh));
}

#[test]
fn a_root_certificate_s_hash_is_its_der_s_sha256_and_no_jwk_s() {
    let root = chain::chain(IAT).pop().unwrap();
    let expected: [u8; 32] = sha2::Sha256::digest(&root).into();
    assert_eq!(pki::root_hash(&root), expected);
    // The same key as a JWK hashes otherwise.
    let public = key(ROOT_SEED).to_public_key();
    let jwk = Jwk {
        kid: "root".into(),
        n: rsa::traits::PublicKeyParts::n(&public).to_bytes_be(),
        e: rsa::traits::PublicKeyParts::e(&public).to_bytes_be(),
    };
    assert_ne!(jwk.key_hash(), pki::root_hash(&root));
}

#[test]
fn the_test_root_reads_back_from_its_pem_as_its_der() {
    let pem = chain::root_pem(IAT);
    assert_eq!(
        pki::pem_der(&pem).unwrap(),
        chain::chain(IAT).pop().unwrap()
    );
    assert_eq!(
        pki::pem_der("not a certificate"),
        Err(Error::Malformed("certificate"))
    );
}

#[test]
fn a_pki_token_s_chain_is_checked_before_its_nonce_which_stays_the_last_check() {
    // An expired intermediate and a binding the nonce does not commit to:
    // the chain is what is refused, so a caller that takes a refused nonce
    // to mean every other check passed is right to.
    let expired = Spec {
        not_after: IAT - 1,
        ..intermediate()
    };
    let chain = chain::chain_of(&leaf(), &expired, &root());
    let input = Input {
        token: chain::token(&chain, &key(LEAF_SEED), &claims(&binding(), IAT)),
        keys: Vec::new(),
        now: NOW,
        binding: Binding::default(),
    };
    assert_eq!(verify(&input), Err(Error::Chain("validity")));
    let mut good = chain::input(&binding(), IAT, NOW);
    good.binding = Binding::default();
    assert_eq!(verify(&good), Err(Error::Nonce));
}
