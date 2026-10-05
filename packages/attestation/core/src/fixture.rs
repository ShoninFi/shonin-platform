//! Signed test tokens in Google's format.
//!
//! Google publishes the token's format and its signing keys, but no signed
//! sample token: a real one exists only inside a Confidential Space VM. So
//! these tokens carry the claims a real one carries, in the same shapes,
//! signed with RS256 under a 2048-bit test key that only tests trust,
//! generated from a fixed public seed. A checker deployed for real trusts
//! Google's keys, never this one.

use alloc::string::String;
use alloc::vec::Vec;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rsa::pkcs1v15::SigningKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::{Value, json};
use sha2::Sha256;

use crate::{AUDIENCE, Binding, DEBUG_DISABLED, HARDWARE, ISSUER, Input, Jwk, SOFTWARE};

/// The seed the test key is generated from. Public on purpose: the key
/// signs nothing but fixtures.
pub const TEST_KEY_SEED: [u8; 32] = *b"shonin attestation fixture key 1";
/// The `kid` the test key is published under.
pub const TEST_KID: &str = "shonin-test-signing-key";
pub use crate::GOOGLE_JWKS_2026_10_06;

/// The image digest the sample token names.
pub const IMAGE_DIGEST: &str =
    "sha256:3b9e5f2a6c1d4e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7";
/// The GCE project the sample instance runs in.
pub const PROJECT: &str = "shonin-executor-pool-1";

/// The test key, the same every time (generated once per process).
pub fn test_private_key() -> RsaPrivateKey {
    use rand_chacha::ChaCha20Rng;
    use rand_chacha::rand_core::SeedableRng;
    static KEY: std::sync::OnceLock<RsaPrivateKey> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        RsaPrivateKey::new(&mut ChaCha20Rng::from_seed(TEST_KEY_SEED), 2048)
            .expect("the test key generates")
    })
    .clone()
}

/// The test key as a JWKS entry.
pub fn test_jwk() -> Jwk {
    let public = RsaPublicKey::from(&test_private_key());
    Jwk {
        kid: TEST_KID.into(),
        n: public.n().to_bytes_be(),
        e: public.e().to_bytes_be(),
    }
}

/// The claims of a production token from a Confidential Space VM on Intel
/// TDX, issued at `iat` for an hour, its nonce binding `binding`.
pub fn claims(binding: &Binding, iat: u64) -> Value {
    json!({
        "aud": AUDIENCE,
        "iss": ISSUER,
        "sub": format!(
            "https://www.googleapis.com/compute/v1/projects/{PROJECT}/zones/us-central1-a/instances/executor-1"
        ),
        "iat": iat,
        "nbf": iat,
        "exp": iat + 3600,
        "dbgstat": DEBUG_DISABLED,
        "eat_nonce": [binding.nonce()],
        "eat_profile": "https://cloud.google.com/confidential-computing/confidential-space/docs/reference/token-claims",
        "google_service_accounts": [format!("executor@{PROJECT}.iam.gserviceaccount.com")],
        "hwmodel": HARDWARE,
        "oemid": 11129,
        "secboot": true,
        "swname": SOFTWARE,
        "swversion": ["25090100"],
        "submods": {
            "confidential_space": {
                "monitoring_enabled": {"memory": false},
                "support_attributes": ["LATEST", "STABLE", "USABLE"]
            },
            "container": {
                "args": ["/usr/local/bin/executor"],
                "env": {
                    "HOSTNAME": "executor-1",
                    "PATH": "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
                },
                "image_digest": IMAGE_DIGEST,
                "image_id": "sha256:0d1e2f3a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f6",
                "image_reference": "us-docker.pkg.dev/shonin/executor/executor@sha256:3b9e5f2a6c1d4e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7",
                "restart_policy": "Never"
            },
            "gce": {
                "instance_id": "1234567890123456789",
                "instance_name": "executor-1",
                "project_id": PROJECT,
                "project_number": "123456789012",
                "zone": "us-central1-a"
            }
        }
    })
}

/// The header Google's tokens carry, naming `kid`.
pub fn header(kid: &str) -> Value {
    json!({"alg": "RS256", "kid": kid, "typ": "JWT"})
}

/// A compact JWT of `header` and `claims`, signed with RS256 under `key`.
pub fn sign_with(key: &RsaPrivateKey, header: &Value, claims: &Value) -> String {
    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap()),
    );
    let signature: Vec<u8> = SigningKey::<Sha256>::new(key.clone())
        .sign(signing_input.as_bytes())
        .to_vec();
    format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature))
}

/// `claims` signed under the test key.
pub fn sign(claims: &Value) -> String {
    sign_with(&test_private_key(), &header(TEST_KID), claims)
}

/// The chain the sample vault lives on, and the one it is blessed on:
/// Foundry's default chain id, so a contract test can bless with it.
pub const CHAIN_ID: u64 = 31_337;
/// The sample vault.
pub const VAULT: [u8; 20] = [0x0A; 20];
/// The sample vault's bundle.
pub const STRATEGY_HASH: [u8; 32] = [0x5B; 32];

/// A sample binding: an EVM key, a Solana key, and as the vaults root the
/// one leaf of the sample vault on [`CHAIN_ID`] (a root over one leaf is the
/// leaf, proved with an empty proof).
pub fn binding() -> Binding {
    let chain = crate::chain_word(CHAIN_ID);
    Binding {
        evm_signer: [0xE7; 20],
        solana_signer: [0x5A; 32],
        vaults_root: crate::blessing_leaf(
            &chain,
            &crate::address_word(&VAULT),
            &STRATEGY_HASH,
            &chain,
        ),
    }
}

/// A guest input that passes: a token issued at `iat`, checked at `now`,
/// against Google's published keys and the test key.
pub fn input(binding: &Binding, iat: u64, now: u64) -> Input {
    let mut keys = Jwk::from_jwks(GOOGLE_JWKS_2026_10_06).expect("Google's JWKS parses");
    keys.push(test_jwk());
    Input {
        token: sign(&claims(binding, iat)),
        keys,
        now,
        binding: *binding,
    }
}
