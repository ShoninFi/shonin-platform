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

/// An EVM registry's blessing leaf, which names the key it blesses:
/// `keccak256(keccak256(abi.encode(vaultChain, vault, strategyHash,
/// thisChain, key)))`, five words.
pub fn key_leaf(
    vault_chain: u64,
    vault: &[u8; 20],
    strategy_hash: &[u8; 32],
    this_chain: u64,
    key: &[u8; 20],
) -> [u8; 32] {
    use sha3::{Digest, Keccak256};
    let mut hasher = Keccak256::new();
    hasher.update(crate::chain_word(vault_chain));
    hasher.update(crate::address_word(vault));
    hasher.update(strategy_hash);
    hasher.update(crate::chain_word(this_chain));
    hasher.update(crate::address_word(key));
    let inner: [u8; 32] = hasher.finalize().into();
    Keccak256::digest(inner).into()
}

/// The sample instance's EVM key.
pub const EVM_SIGNER: [u8; 20] = [0xE7; 20];

/// A sample binding: an EVM key, a Solana key, and as the vaults root the
/// one leaf blessing that EVM key for the sample vault on [`CHAIN_ID`] (a
/// root over one leaf is the leaf, proved with an empty proof).
pub fn binding() -> Binding {
    Binding {
        evm_signer: EVM_SIGNER,
        solana_signer: [0x5A; 32],
        vaults_root: key_leaf(CHAIN_ID, &VAULT, &STRATEGY_HASH, CHAIN_ID, &EVM_SIGNER),
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

/// A genuine token from a Confidential Space VM (Intel TDX, the hardened
/// launcher image `260900`, project `shonin-testnet`), issued on 6 October
/// 2026 and signed by Google's key in [`GOOGLE_JWKS_2026_10_06`]. It carries
/// no secret: its nonce binds a key made for this fixture and a one-leaf
/// root ([`real_binding`]). Proof that the checks accept what Confidential
/// Space really issues, not only what its documentation describes.
pub const REAL_TOKEN: &str = include_str!("../testdata/cs-token-oidc-2026-10-06.jwt");
/// The same claims from the same VM in Google's PKI format: an `x5c` chain
/// in the header (leaf, intermediate, Google's Confidential Space Root CA),
/// signed by the leaf's key, whose `kid` is among no published keys.
pub const REAL_PKI_TOKEN: &str = include_str!("../testdata/cs-token-pki-2026-10-06.jwt");
/// The real PKI token's `iat` (a second after the OIDC token's); it
/// expires an hour later.
pub const REAL_PKI_IAT: u64 = 1_791_320_212;
/// `sha256` of Google's Confidential Space Root CA certificate (DER), as
/// `openssl x509 -outform der | sha256sum` gives it.
pub const CONFIDENTIAL_SPACE_ROOT_HASH: &str =
    "148b293821bb0c6a317f413c8ba475814091cb22d49b9e3c94198db8e8f86c39";
/// The real token's `iat`; it expires an hour later, at [`REAL_EXP`].
pub const REAL_IAT: u64 = 1_791_320_211;
pub const REAL_EXP: u64 = REAL_IAT + 3600;
/// The real token's image digest and project.
pub const REAL_IMAGE_DIGEST: &str =
    "sha256:b6192f4f8125235df12b18c764b6ecd068ccbe223cfd5dff24c6934bd3704342";
pub const REAL_PROJECT: &str = "shonin-testnet";
/// The hash ([`Jwk::key_hash`]) of Google's key that signed the real token.
pub const REAL_KEY_HASH: &str = "8c87b83d047d492d8a05ebcc84333ff23a5c03d8ebfbd0c5bd88aeca78bbf8eb";
/// The real token's vault: one leaf, a vault on [`CHAIN_ID`] blessed there.
pub const REAL_VAULT: [u8; 20] = [0x11; 20];
/// `keccak256("shonin fixture strategy")`.
pub const REAL_STRATEGY_HASH: &str =
    "d3b76bf6803fde2a360c1efa4b1c8e2e3b79cb80509ade1c0bb70d72c155456e";
/// The EVM key the real token's nonce binds (made for the fixture, never
/// funded; only its address is kept).
pub const REAL_EVM_SIGNER: &str = "e890cd248bed3547dc084cfb22c62da62f410539";

fn hex_array<const N: usize>(text: &str) -> [u8; N] {
    hex::decode(text)
        .expect("fixture hex")
        .try_into()
        .expect("fixture length")
}

/// What the real token's nonce binds: the fixture's EVM key, no Solana key,
/// and the root of the one leaf of [`REAL_VAULT`].
pub fn real_binding() -> Binding {
    let chain = crate::chain_word(CHAIN_ID);
    Binding {
        evm_signer: hex_array(REAL_EVM_SIGNER),
        solana_signer: [0; 32],
        vaults_root: crate::blessing_leaf(
            &chain,
            &crate::address_word(&REAL_VAULT),
            &hex_array(REAL_STRATEGY_HASH),
            &chain,
        ),
    }
}

/// A guest input over the real token, checked at `now` against Google's
/// keys as published on 6 October 2026 (and nothing else).
pub fn real_input(now: u64) -> Input {
    Input {
        token: REAL_TOKEN.into(),
        keys: Jwk::from_jwks(GOOGLE_JWKS_2026_10_06).expect("Google's JWKS parses"),
        now,
        binding: real_binding(),
    }
}

/// A certificate chain of the test's own, in the shape Google's PKI tokens
/// carry (a leaf, an intermediate CA, a root CA), its keys generated from
/// fixed public seeds like the test key. A checker deployed for real pins
/// Google's root, never this one.
pub mod chain {
    use alloc::string::String;
    use alloc::vec::Vec;
    use core::str::FromStr;
    use core::time::Duration;

    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
    use rsa::pkcs1v15::SigningKey;
    use rsa::pkcs8::EncodePublicKey;
    use rsa::signature::{SignatureEncoding, Signer};
    use rsa::{RsaPrivateKey, RsaPublicKey};
    use serde_json::{Value, json};
    use sha2::Sha256;
    use x509_cert::certificate::{Certificate, TbsCertificate, Version};
    use x509_cert::der::asn1::{Any, BitString, ObjectIdentifier, OctetString, UtcTime};
    use x509_cert::der::{Decode, Encode};
    use x509_cert::ext::Extension;
    use x509_cert::ext::pkix::{BasicConstraints, KeyUsage, KeyUsages};
    use x509_cert::name::Name;
    use x509_cert::serial_number::SerialNumber;
    use x509_cert::spki::{AlgorithmIdentifierOwned, SubjectPublicKeyInfoOwned};
    use x509_cert::time::{Time, Validity};

    use crate::{Binding, Input};

    /// The seeds the chain's keys are generated from. Public on purpose:
    /// they sign nothing but fixtures.
    pub const ROOT_SEED: [u8; 32] = *b"shonin attestation test root  01";
    pub const INTERMEDIATE_SEED: [u8; 32] = *b"shonin attestation test interm01";
    pub const LEAF_SEED: [u8; 32] = *b"shonin attestation test leaf  01";
    /// Another root, which no checker pins.
    pub const OTHER_ROOT_SEED: [u8; 32] = *b"shonin attestation other root 01";

    const SEEDS: [[u8; 32]; 4] = [ROOT_SEED, INTERMEDIATE_SEED, LEAF_SEED, OTHER_ROOT_SEED];

    /// The key generated from `seed` (one of the seeds above), the same
    /// every time and generated once per process.
    pub fn key(seed: [u8; 32]) -> RsaPrivateKey {
        use rand_chacha::ChaCha20Rng;
        use rand_chacha::rand_core::SeedableRng;
        static KEYS: [std::sync::OnceLock<RsaPrivateKey>; 4] = [
            std::sync::OnceLock::new(),
            std::sync::OnceLock::new(),
            std::sync::OnceLock::new(),
            std::sync::OnceLock::new(),
        ];
        let at = SEEDS
            .iter()
            .position(|known| *known == seed)
            .expect("one of the fixture's seeds");
        KEYS[at]
            .get_or_init(|| {
                RsaPrivateKey::new(&mut ChaCha20Rng::from_seed(seed), 2048)
                    .expect("the test key generates")
            })
            .clone()
    }

    pub const ROOT_NAME: &str = "CN=Shonin Test Root CA,O=Shonin Test";
    pub const INTERMEDIATE_NAME: &str = "CN=Shonin Test Intermediate CA,O=Shonin Test";
    pub const LEAF_NAME: &str = "CN=Shonin Test Leaf Certificate,O=Shonin Test";

    /// `sha256WithRSAEncryption`.
    pub const SHA256_WITH_RSA: ObjectIdentifier =
        ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
    /// An extension no checker knows (a private arc).
    pub const UNKNOWN_EXTENSION: ObjectIdentifier =
        ObjectIdentifier::new_unwrap("1.3.6.1.4.1.99999.1");

    /// What one certificate says.
    #[derive(Clone, Debug)]
    pub struct Spec {
        pub subject: &'static str,
        pub issuer: &'static str,
        /// Valid from and until (inclusive), Unix seconds.
        pub not_before: u64,
        pub not_after: u64,
        /// Basic constraints, if present: `cA` and the path length
        /// constraint.
        pub basic: Option<(bool, Option<u8>)>,
        /// Key usage, if present.
        pub usage: Option<x509_cert::der::flagset::FlagSet<KeyUsages>>,
        /// An extension no checker knows, marked critical.
        pub unknown_critical: bool,
        /// The signature algorithm named (sha256WithRSAEncryption unless
        /// a test says otherwise).
        pub algorithm: ObjectIdentifier,
    }

    impl Spec {
        /// A root CA, valid for twenty years around `iat`.
        pub fn root(iat: u64) -> Spec {
            Spec {
                subject: ROOT_NAME,
                issuer: ROOT_NAME,
                not_before: iat - 10 * 365 * 86_400,
                not_after: iat + 10 * 365 * 86_400,
                basic: Some((true, None)),
                usage: Some(KeyUsages::KeyCertSign | KeyUsages::CRLSign),
                unknown_critical: false,
                algorithm: SHA256_WITH_RSA,
            }
        }

        /// An intermediate CA, valid for three years around `iat`.
        pub fn intermediate(iat: u64) -> Spec {
            Spec {
                subject: INTERMEDIATE_NAME,
                issuer: ROOT_NAME,
                not_before: iat - 365 * 86_400,
                not_after: iat + 2 * 365 * 86_400,
                ..Spec::root(iat)
            }
        }

        /// A leaf as Google issues them: not a CA, no key usage, valid for
        /// sixty days from a few days before `iat`.
        pub fn leaf(iat: u64) -> Spec {
            Spec {
                subject: LEAF_NAME,
                issuer: INTERMEDIATE_NAME,
                not_before: iat - 3 * 86_400,
                not_after: iat + 57 * 86_400,
                basic: Some((false, None)),
                usage: None,
                unknown_critical: false,
                algorithm: SHA256_WITH_RSA,
            }
        }
    }

    fn time(seconds: u64) -> Time {
        Time::UtcTime(UtcTime::from_unix_duration(Duration::from_secs(seconds)).expect("a time"))
    }

    fn extension(oid: ObjectIdentifier, critical: bool, value: Vec<u8>) -> Extension {
        Extension {
            extn_id: oid,
            critical,
            extn_value: OctetString::new(value).expect("an extension value"),
        }
    }

    /// A certificate of `subject`'s key as `spec` says, signed by `issuer`.
    pub fn certificate(spec: &Spec, subject: &RsaPublicKey, issuer: &RsaPrivateKey) -> Vec<u8> {
        let algorithm = AlgorithmIdentifierOwned {
            oid: spec.algorithm,
            parameters: Some(Any::null()),
        };
        let mut extensions = Vec::new();
        if let Some((ca, path_len_constraint)) = spec.basic {
            let value = BasicConstraints {
                ca,
                path_len_constraint,
            };
            extensions.push(extension(
                ObjectIdentifier::new_unwrap("2.5.29.19"),
                true,
                value.to_der().unwrap(),
            ));
        }
        if let Some(usage) = spec.usage {
            extensions.push(extension(
                ObjectIdentifier::new_unwrap("2.5.29.15"),
                true,
                KeyUsage(usage).to_der().unwrap(),
            ));
        }
        if spec.unknown_critical {
            extensions.push(extension(UNKNOWN_EXTENSION, true, alloc::vec![0x05, 0x00]));
        }
        let spki = subject.to_public_key_der().expect("an RSA key encodes");
        let tbs = TbsCertificate {
            version: Version::V3,
            serial_number: SerialNumber::new(&[0x01, 0x23, 0x45]).unwrap(),
            signature: algorithm.clone(),
            issuer: Name::from_str(spec.issuer).unwrap(),
            validity: Validity {
                not_before: time(spec.not_before),
                not_after: time(spec.not_after),
            },
            subject: Name::from_str(spec.subject).unwrap(),
            subject_public_key_info: SubjectPublicKeyInfoOwned::from_der(spki.as_bytes()).unwrap(),
            issuer_unique_id: None,
            subject_unique_id: None,
            extensions: Some(extensions),
        };
        let signature: Vec<u8> = SigningKey::<Sha256>::new(issuer.clone())
            .sign(&tbs.to_der().unwrap())
            .to_vec();
        Certificate {
            tbs_certificate: tbs,
            signature_algorithm: algorithm,
            signature: BitString::from_bytes(&signature).unwrap(),
        }
        .to_der()
        .unwrap()
    }

    /// The test chain, as Google's: leaf, intermediate, root, each as its
    /// spec says, each signed by the next (the root by itself).
    pub fn chain_of(leaf: &Spec, intermediate: &Spec, root: &Spec) -> Vec<Vec<u8>> {
        let (root_key, intermediate_key, leaf_key) =
            (key(ROOT_SEED), key(INTERMEDIATE_SEED), key(LEAF_SEED));
        alloc::vec![
            certificate(leaf, &leaf_key.to_public_key(), &intermediate_key),
            certificate(intermediate, &intermediate_key.to_public_key(), &root_key),
            certificate(root, &root_key.to_public_key(), &root_key),
        ]
    }

    /// The test chain for a token issued at `iat`.
    pub fn chain(iat: u64) -> Vec<Vec<u8>> {
        chain_of(&Spec::leaf(iat), &Spec::intermediate(iat), &Spec::root(iat))
    }

    /// What a checker pins to accept tokens chained to the test root.
    pub fn root_hash(iat: u64) -> [u8; 32] {
        crate::pki::root_hash(chain(iat).last().unwrap())
    }

    /// The test root as a PEM certificate, as a prover is given a root.
    pub fn root_pem(iat: u64) -> String {
        let der = STANDARD.encode(chain(iat).pop().unwrap());
        let lines: Vec<&str> = der
            .as_bytes()
            .chunks(64)
            .map(|line| core::str::from_utf8(line).unwrap())
            .collect();
        format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            lines.join("\n")
        )
    }

    /// A PKI token's header carrying `chain`.
    pub fn header(chain: &[Vec<u8>]) -> Value {
        let x5c: Vec<String> = chain.iter().map(|der| STANDARD.encode(der)).collect();
        json!({"alg": "RS256", "kid": "shonin-test-leaf", "typ": "JWT", "x5c": x5c})
    }

    /// `claims` as a PKI token carrying `chain`, signed by `signer`.
    pub fn token(chain: &[Vec<u8>], signer: &RsaPrivateKey, claims: &Value) -> String {
        super::sign_with(signer, &header(chain), claims)
    }

    /// A guest input that passes: a PKI token issued at `iat` under the test
    /// chain, checked at `now`. No JWKS key is given: the chain is the
    /// token's own.
    pub fn input(binding: &Binding, iat: u64, now: u64) -> Input {
        Input {
            token: token(&chain(iat), &key(LEAF_SEED), &super::claims(binding, iat)),
            keys: Vec::new(),
            now,
            binding: *binding,
        }
    }

    /// The header's `x5c` of a compact token, decoded.
    pub fn x5c_of(token: &str) -> Vec<Vec<u8>> {
        let header = token.split('.').next().unwrap();
        let header: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).unwrap()).unwrap();
        header["x5c"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| STANDARD.decode(item.as_str().unwrap()).unwrap())
            .collect()
    }
}
