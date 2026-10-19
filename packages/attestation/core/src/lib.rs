//! What the attestation proof checks, and what it says when it passes.
//!
//! An instance running in Confidential Space asks Google for an attestation
//! token: a JWT, signed with RS256, whose claims describe the hardware, the
//! boot, the container image and the project the instance runs in, and carry
//! a nonce the instance chose. An OIDC token is signed by a key Google
//! publishes in its JWKS; a PKI token by a short-lived key whose certificate
//! chain, up to Google's Confidential Space Root CA, travels in its header
//! (see [`pki`]). The zkVM guest runs [`verify`] over that token and commits
//! the [`Journal`] it returns, so a chain can believe the claims without
//! checking an RSA signature or parsing JSON itself.
//!
//! The same code runs natively, which is how its checks are unit-tested.
//!
//! # The journal
//!
//! One proof serves both kinds of chain. The guest commits one canonical
//! journal, [`Journal::LEN`] bytes, packed big-endian:
//!
//! | offset | bytes | field |
//! | --- | --- | --- |
//! | 0 | 32 | image digest (the container's `sha256` digest) |
//! | 32 | 32 | project id (`keccak256` of the GCE project id) |
//! | 64 | 32 | vaults root (the Merkle root of the instance's blessing leaves) |
//! | 96 | 8 | issued at (the token's `iat`, seconds, big-endian) |
//! | 104 | 20 | the instance's EVM key (an address; zero if it has none) |
//! | 124 | 32 | the instance's Solana key (ed25519; zero if it has none) |
//! | 156 | 32 | what the token's signature is trusted by: the Google key that signed it ([`Jwk::key_hash`]), or a PKI token's root certificate ([`pki::root_hash`]) |
//!
//! Each chain's registry hands its checker its own journal (160 bytes of
//! `abi.encode` on an EVM chain, 136 bytes of Borsh on Solana). The checker
//! rebuilds the canonical journal from it, with the other chain's key and
//! that last hash carried after the seal, checks the hash against the ones
//! it trusts, and verifies the seal against `sha256` of the result. So
//! neither registry changes, and the extra bytes on the wire are 52 on
//! Solana and 64 on an EVM chain.

#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::signature::Verifier;
use rsa::{BigUint, RsaPublicKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sha3::Keccak256;

/// Who issues Confidential Space attestation tokens.
pub const ISSUER: &str = "https://confidentialcomputing.googleapis.com";
/// The audience a Shonin instance asks for in its custom token, so a token
/// requested for any other relying party is never accepted here.
pub const AUDIENCE: &str = "https://shonin.fi/attestation";
/// A production image: a debug image lets the host read memory, and so keys.
pub const DEBUG_DISABLED: &str = "disabled-since-boot";
/// The Confidential Space launcher, as opposed to any other software.
pub const SOFTWARE: &str = "CONFIDENTIAL_SPACE";
/// The hardware root: Intel TDX. Launch uses a single vendor.
pub const HARDWARE: &str = "GCP_INTEL_TDX";
/// Google's own mark of a Confidential Space image version that is supported
/// and receives security fixes, which a fixed minimum version would not
/// follow as versions age out.
pub const SUPPORTED: &str = "STABLE";
/// The smallest RSA key accepted, in bits.
pub const MIN_KEY_BITS: usize = 2048;
/// What the instance's nonce commits to is hashed under this tag, so it can
/// never be mistaken for any other hash.
pub const NONCE_TAG: &[u8] = b"shonin-attestation-nonce:v1";

/// Google's Confidential Space JWKS as published on 6 October 2026, from the
/// `jwks_uri` of the issuer's OpenID configuration
/// (`https://www.googleapis.com/service_accounts/v1/metadata/jwk/signer@confidentialspace-sign.iam.gserviceaccount.com`).
/// Google rotates these keys; a checker is deployed with the keys published
/// when it is.
pub const GOOGLE_JWKS_2026_10_06: &str = include_str!("../testdata/google-jwks-2026-10-06.json");

/// The Google key set pinned now: what a checker is deployed trusting (on
/// every EVM chain and on Solana) and what the prover checks tokens against
/// by default. Pinning a newer set is a new `google-jwks-<date>.json` beside
/// this one and this line naming it; the scheduled key check reads every
/// such file.
pub const PINNED_GOOGLE_JWKS: &str = include_str!("../testdata/google-jwks-2026-10-06.json");

/// Google's Confidential Space Root CA (RSA-4096, valid from 19 January
/// 2024 to 16 January 2034), as Google publishes it at
/// `https://confidentialcomputing.googleapis.com/.well-known/confidential_space_root.crt`.
/// A PKI token's chain ends in it; a checker pins its hash
/// ([`confidential_space_root_hash`]).
pub const CONFIDENTIAL_SPACE_ROOT: &str = include_str!("../testdata/confidential-space-root.crt");

/// What a checker pins to accept PKI tokens whose chain ends in Google's
/// Confidential Space Root CA: `sha256` of the certificate's DER
/// ([`pki::root_hash`]).
pub fn confidential_space_root_hash() -> [u8; 32] {
    pki::root_hash(
        &pki::pem_der(CONFIDENTIAL_SPACE_ROOT).expect("the pinned root is a PEM certificate"),
    )
}

/// One of Google's signing keys, as published in its JWKS.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Jwk {
    pub kid: String,
    /// The modulus, big-endian.
    pub n: Vec<u8>,
    /// The public exponent, big-endian.
    pub e: Vec<u8>,
}

impl Jwk {
    /// What the journal names the key by, and what a checker trusts:
    /// `sha256(len(n) ‖ n ‖ len(e) ‖ e)`, each length a big-endian u32 and
    /// each number without leading zeros. The `kid` is a label, not the key,
    /// so it is not part of it.
    pub fn key_hash(&self) -> [u8; 32] {
        let n = strip_zeros(&self.n);
        let e = strip_zeros(&self.e);
        let mut hasher = Sha256::new();
        hasher.update((n.len() as u32).to_be_bytes());
        hasher.update(n);
        hasher.update((e.len() as u32).to_be_bytes());
        hasher.update(e);
        hasher.finalize().into()
    }

    /// The RSA keys of a JWKS document (`{"keys": [...]}`); anything else
    /// in it is skipped.
    pub fn from_jwks(json: &str) -> Result<Vec<Jwk>, Error> {
        let doc: Value = serde_json::from_str(json).map_err(|_| Error::Malformed("jwks"))?;
        let keys = doc
            .get("keys")
            .and_then(Value::as_array)
            .ok_or(Error::Malformed("jwks"))?;
        let mut out = Vec::new();
        for key in keys {
            if key.get("kty").and_then(Value::as_str) != Some("RSA") {
                continue;
            }
            let field = |name: &'static str| -> Result<&str, Error> {
                key.get(name)
                    .and_then(Value::as_str)
                    .ok_or(Error::Malformed("jwk"))
            };
            out.push(Jwk {
                kid: field("kid")?.into(),
                n: b64(field("n")?)?,
                e: b64(field("e")?)?,
            });
        }
        Ok(out)
    }
}

fn strip_zeros(bytes: &[u8]) -> &[u8] {
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
    &bytes[first..]
}

/// What the instance binds into its token's nonce: the keys it generated and
/// the root of the vaults it holds them for. Nothing in the token says this
/// otherwise, so it is what makes the token about these keys.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Binding {
    /// The instance's EVM key, an address; zero if it serves no EVM chain.
    pub evm_signer: [u8; 20],
    /// The instance's Solana key; zero if it serves no Solana cluster.
    pub solana_signer: [u8; 32],
    /// The Merkle root of its `(vault chain, vault, bundle, chain)` leaves.
    pub vaults_root: [u8; 32],
}

impl Binding {
    /// The nonce the instance asks for in its token: the lowercase hex of
    /// `sha256(NONCE_TAG ‖ evm_signer ‖ solana_signer ‖ vaults_root)`, 64
    /// characters (Confidential Space takes nonces of 8 to 88 bytes).
    pub fn nonce(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(NONCE_TAG);
        hasher.update(self.evm_signer);
        hasher.update(self.solana_signer);
        hasher.update(self.vaults_root);
        hex::encode(hasher.finalize())
    }
}

/// What the guest reads.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Input {
    /// The attestation token, as the compact JWT the instance was given.
    pub token: String,
    /// The signing keys to check an OIDC token against: Google's JWKS at
    /// the time. The one that signed is named in the journal by its hash,
    /// and only a checker that trusts that hash accepts the proof. A PKI
    /// token carries its own chain, and these are not used for it.
    pub keys: Vec<Jwk>,
    /// The prover's clock, in seconds. The token must be valid at it. The
    /// chain judges freshness by the token's `iat` against its own clock.
    pub now: u64,
    pub binding: Binding,
}

/// What the proof says, once the token has passed every check.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Journal {
    pub image_digest: [u8; 32],
    pub project_id: [u8; 32],
    pub vaults_root: [u8; 32],
    pub issued_at: u64,
    pub evm_signer: [u8; 20],
    pub solana_signer: [u8; 32],
    /// What the token's signature is trusted by: the signing key's
    /// [`Jwk::key_hash`] (an OIDC token), or its chain's root certificate's
    /// [`pki::root_hash`] (a PKI token). The two hash different kinds of
    /// bytes, so neither can pass for the other.
    pub key_hash: [u8; 32],
}

impl Journal {
    /// The canonical journal's length in bytes.
    pub const LEN: usize = 188;
    /// The length of the journal an EVM registry reads.
    pub const EVM_LEN: usize = 160;
    /// The length of the journal the Solana registry reads.
    pub const SOLANA_LEN: usize = 136;

    /// The canonical bytes the guest commits (see the crate's docs).
    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0..32].copy_from_slice(&self.image_digest);
        out[32..64].copy_from_slice(&self.project_id);
        out[64..96].copy_from_slice(&self.vaults_root);
        out[96..104].copy_from_slice(&self.issued_at.to_be_bytes());
        out[104..124].copy_from_slice(&self.evm_signer);
        out[124..156].copy_from_slice(&self.solana_signer);
        out[156..188].copy_from_slice(&self.key_hash);
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Journal> {
        if bytes.len() != Self::LEN {
            return None;
        }
        let word = |at: usize| -> [u8; 32] { bytes[at..at + 32].try_into().unwrap() };
        Some(Journal {
            image_digest: word(0),
            project_id: word(32),
            vaults_root: word(64),
            issued_at: u64::from_be_bytes(bytes[96..104].try_into().unwrap()),
            evm_signer: bytes[104..124].try_into().unwrap(),
            solana_signer: word(124),
            key_hash: word(156),
        })
    }

    /// `sha256` of the canonical bytes: what a RISC Zero seal is verified
    /// against.
    pub fn digest(&self) -> [u8; 32] {
        Sha256::digest(self.encode()).into()
    }

    /// The journal an EVM registry reads: `abi.encode(imageDigest,
    /// projectId, signer, vaultsRoot, uint64 issuedAt)`, 160 bytes.
    pub fn evm(&self) -> [u8; Self::EVM_LEN] {
        let mut out = [0u8; Self::EVM_LEN];
        out[0..32].copy_from_slice(&self.image_digest);
        out[32..64].copy_from_slice(&self.project_id);
        out[76..96].copy_from_slice(&self.evm_signer);
        out[96..128].copy_from_slice(&self.vaults_root);
        out[152..160].copy_from_slice(&self.issued_at.to_be_bytes());
        out
    }

    /// The journal the Solana registry reads: Borsh of the image digest, the
    /// project id, the signer, the vaults root and `issued_at` (u64,
    /// little-endian), 136 bytes.
    pub fn solana(&self) -> [u8; Self::SOLANA_LEN] {
        let mut out = [0u8; Self::SOLANA_LEN];
        out[0..32].copy_from_slice(&self.image_digest);
        out[32..64].copy_from_slice(&self.project_id);
        out[64..96].copy_from_slice(&self.solana_signer);
        out[96..128].copy_from_slice(&self.vaults_root);
        out[128..136].copy_from_slice(&self.issued_at.to_le_bytes());
        out
    }

    /// What an EVM checker reads after the seal: the Solana key and the
    /// hash the token is trusted by.
    pub fn evm_seal_tail(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        out[..32].copy_from_slice(&self.solana_signer);
        out[32..].copy_from_slice(&self.key_hash);
        out
    }

    /// The canonical journal an EVM checker rebuilds from the registry's
    /// journal and what follows the seal. `None` if either is the wrong
    /// length or the registry's journal is not a canonical `abi.encode`.
    pub fn from_evm(evm: &[u8], tail: &[u8]) -> Option<Journal> {
        if evm.len() != Self::EVM_LEN || tail.len() != 64 {
            return None;
        }
        // The address and the u64 fill the low bytes of their words; the
        // rest must be zero, as `abi.decode` requires.
        if evm[64..76].iter().any(|b| *b != 0) || evm[128..152].iter().any(|b| *b != 0) {
            return None;
        }
        Some(Journal {
            image_digest: evm[0..32].try_into().ok()?,
            project_id: evm[32..64].try_into().ok()?,
            evm_signer: evm[76..96].try_into().ok()?,
            vaults_root: evm[96..128].try_into().ok()?,
            issued_at: u64::from_be_bytes(evm[152..160].try_into().ok()?),
            solana_signer: tail[..32].try_into().ok()?,
            key_hash: tail[32..].try_into().ok()?,
        })
    }

    /// The canonical journal the Solana checker rebuilds from the registry's
    /// journal and what follows the seal.
    pub fn from_solana(solana: &[u8], tail: &[u8]) -> Option<Journal> {
        if solana.len() != Self::SOLANA_LEN || tail.len() != 52 {
            return None;
        }
        Some(Journal {
            image_digest: solana[0..32].try_into().ok()?,
            project_id: solana[32..64].try_into().ok()?,
            solana_signer: solana[64..96].try_into().ok()?,
            vaults_root: solana[96..128].try_into().ok()?,
            issued_at: u64::from_le_bytes(solana[128..136].try_into().ok()?),
            evm_signer: tail[..20].try_into().ok()?,
            key_hash: tail[20..].try_into().ok()?,
        })
    }

    /// What the Solana checker reads after the seal: the EVM key and the
    /// hash the token is trusted by.
    pub fn solana_seal_tail(&self) -> [u8; 52] {
        let mut out = [0u8; 52];
        out[..20].copy_from_slice(&self.evm_signer);
        out[20..].copy_from_slice(&self.key_hash);
        out
    }
}

/// The operator project's id as the registries list it: `keccak256` of the
/// GCE project id's bytes.
pub fn project_id(gce_project: &str) -> [u8; 32] {
    Keccak256::digest(gce_project.as_bytes()).into()
}

/// One blessing leaf, the same on every chain:
/// `keccak256(keccak256(vaultChain ‖ vault ‖ strategyHash ‖ thisChain))`
/// over four 32-byte words (an EVM address left-padded, a Solana address as
/// it is). The instance's vaults root is the Merkle root of its leaves,
/// pairs hashed in sorted order.
pub fn blessing_leaf(
    vault_chain: &[u8; 32],
    vault: &[u8; 32],
    strategy_hash: &[u8; 32],
    this_chain: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(vault_chain);
    hasher.update(vault);
    hasher.update(strategy_hash);
    hasher.update(this_chain);
    let inner: [u8; 32] = hasher.finalize().into();
    Keccak256::digest(inner).into()
}

/// A chain id as the 32-byte big-endian word a leaf holds.
pub fn chain_word(chain_id: u64) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[24..].copy_from_slice(&chain_id.to_be_bytes());
    word
}

/// An EVM address as the left-padded word a leaf holds.
pub fn address_word(address: &[u8; 20]) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[12..].copy_from_slice(address);
    word
}

/// Why a token was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not well formed: the part named.
    Malformed(&'static str),
    /// Not signed with RS256.
    Algorithm,
    /// No key with the OIDC token's `kid` was given.
    UnknownKey,
    /// The key is too small, or not a valid RSA key.
    WeakKey,
    BadSignature,
    /// A claim that must hold doesn't: the claim named.
    Claim(&'static str),
    /// Not valid at the prover's clock.
    NotFresh,
    /// The nonce doesn't bind these keys and this root.
    Nonce,
    /// A PKI token's certificate chain is refused: the reason named.
    Chain(&'static str),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Malformed(part) => write!(f, "the token's {part} is malformed"),
            Error::Algorithm => write!(f, "the token is not signed with RS256"),
            Error::UnknownKey => write!(f, "no key was given for the token's kid"),
            Error::WeakKey => write!(f, "the signing key is not an RSA key of at least 2048 bits"),
            Error::BadSignature => write!(f, "the token's signature does not verify"),
            Error::Claim(claim) => write!(f, "the token's `{claim}` claim is not accepted"),
            Error::NotFresh => write!(f, "the token is not valid at the given time"),
            Error::Nonce => write!(f, "the token's nonce does not bind the given keys and root"),
            Error::Chain(why) => write!(f, "the token's certificate chain is refused: {why}"),
        }
    }
}

impl core::error::Error for Error {}

fn b64(text: &str) -> Result<Vec<u8>, Error> {
    URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| Error::Malformed("base64"))
}

/// Check a Confidential Space token as the proof does, and say what it
/// attests to.
pub fn verify(input: &Input) -> Result<Journal, Error> {
    let mut parts = input.token.split('.');
    let (Some(header_b64), Some(payload_b64), Some(signature_b64), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Error::Malformed("structure"));
    };

    let header: Value =
        serde_json::from_slice(&b64(header_b64)?).map_err(|_| Error::Malformed("header"))?;
    if header.get("alg").and_then(Value::as_str) != Some("RS256") {
        return Err(Error::Algorithm);
    }
    let signed = &input.token[..header_b64.len() + 1 + payload_b64.len()];
    let signature = b64(signature_b64)?;

    // A PKI token carries its signing key's certificate chain, and is trusted
    // by the chain's root; any other is signed by one of the keys given.
    let (anchor, chain) = match header.get("x5c") {
        Some(x5c) => {
            let chain = pki::check_chain(&pki::decode_x5c(x5c)?)?;
            verify_rs256(&chain.leaf, signed, &signature)?;
            (chain.root, Some(chain))
        }
        None => {
            let kid = header
                .get("kid")
                .and_then(Value::as_str)
                .ok_or(Error::Malformed("header"))?;
            let key = input
                .keys
                .iter()
                .find(|key| key.kid == kid)
                .ok_or(Error::UnknownKey)?;
            let public = RsaPublicKey::new(
                BigUint::from_bytes_be(&key.n),
                BigUint::from_bytes_be(&key.e),
            )
            .map_err(|_| Error::WeakKey)?;
            if rsa::traits::PublicKeyParts::n(&public).bits() < MIN_KEY_BITS {
                return Err(Error::WeakKey);
            }
            verify_rs256(&public, signed, &signature)?;
            (key.key_hash(), None)
        }
    };

    let claims: Value =
        serde_json::from_slice(&b64(payload_b64)?).map_err(|_| Error::Malformed("claims"))?;
    // Every certificate valid when the token was issued; checked before the
    // claims, so the nonce stays the last check of all (a token without an
    // `iat` is refused by the claims' check).
    if let Some(chain) = chain
        && let Some(iat) = claims.pointer("/iat").and_then(Value::as_u64)
    {
        chain.check_time(iat)?;
    }
    let journal = check_claims(&claims, input.now, &input.binding)?;
    Ok(Journal {
        key_hash: anchor,
        ..journal
    })
}

/// An RS256 (PKCS#1 v1.5, SHA-256) signature over `signed` by `key`.
fn verify_rs256(key: &RsaPublicKey, signed: &str, signature: &[u8]) -> Result<(), Error> {
    let signature = Signature::try_from(signature).map_err(|_| Error::BadSignature)?;
    VerifyingKey::<Sha256>::new(key.clone())
        .verify(signed.as_bytes(), &signature)
        .map_err(|_| Error::BadSignature)
}

fn string<'a>(claims: &'a Value, pointer: &'static str) -> Result<&'a str, Error> {
    claims
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or(Error::Claim(pointer))
}

fn require(claims: &Value, pointer: &'static str, value: &str) -> Result<(), Error> {
    if string(claims, pointer)? == value {
        Ok(())
    } else {
        Err(Error::Claim(pointer))
    }
}

/// Absent, null, or empty: the operator overrode nothing the image says.
fn nothing_overridden(claims: &Value, pointer: &'static str) -> Result<(), Error> {
    match claims.pointer(pointer) {
        None | Some(Value::Null) => Ok(()),
        Some(Value::Array(items)) if items.is_empty() => Ok(()),
        Some(Value::Object(items)) if items.is_empty() => Ok(()),
        Some(_) => Err(Error::Claim(pointer)),
    }
}

/// A claim that is a string, or an array of strings (JWT allows either for
/// `aud`, and Confidential Space uses either for `eat_nonce`).
fn strings<'a>(claims: &'a Value, pointer: &'static str) -> Result<Vec<&'a str>, Error> {
    match claims.pointer(pointer) {
        Some(Value::String(one)) => Ok(alloc::vec![one.as_str()]),
        Some(Value::Array(many)) => many
            .iter()
            .map(|item| item.as_str().ok_or(Error::Claim(pointer)))
            .collect(),
        _ => Err(Error::Claim(pointer)),
    }
}

/// Every claim check, on a token whose signature has already verified. The
/// key hash is left zero for the caller to fill.
fn check_claims(claims: &Value, now: u64, binding: &Binding) -> Result<Journal, Error> {
    require(claims, "/iss", ISSUER)?;
    // Exactly our audience: a token minted for several relying parties could
    // be shown to us by any of them.
    if strings(claims, "/aud")? != [AUDIENCE] {
        return Err(Error::Claim("/aud"));
    }

    let time = |pointer: &'static str| claims.pointer(pointer).and_then(Value::as_u64);
    let iat = time("/iat").ok_or(Error::Claim("/iat"))?;
    let exp = time("/exp").ok_or(Error::Claim("/exp"))?;
    if iat > now || now >= exp {
        return Err(Error::NotFresh);
    }
    if let Some(nbf) = claims.pointer("/nbf")
        && nbf.as_u64().is_none_or(|nbf| nbf > now)
    {
        return Err(Error::NotFresh);
    }

    require(claims, "/dbgstat", DEBUG_DISABLED)?;
    require(claims, "/swname", SOFTWARE)?;
    require(claims, "/hwmodel", HARDWARE)?;
    if claims.pointer("/secboot").and_then(Value::as_bool) != Some(true) {
        return Err(Error::Claim("/secboot"));
    }
    if !strings(claims, "/submods/confidential_space/support_attributes")?.contains(&SUPPORTED) {
        return Err(Error::Claim(
            "/submods/confidential_space/support_attributes",
        ));
    }

    // The image's own arguments and environment are part of its digest; an
    // operator's overrides are not, and an honest image run with other
    // arguments is not the image that was allowlisted.
    nothing_overridden(claims, "/submods/container/cmd_override")?;
    nothing_overridden(claims, "/submods/container/env_override")?;
    let digest = string(claims, "/submods/container/image_digest")?;
    let image_digest: [u8; 32] = digest
        .strip_prefix("sha256:")
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .and_then(|hex| hex::decode(hex).ok())
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(Error::Claim("/submods/container/image_digest"))?;

    let project = string(claims, "/submods/gce/project_id")?;
    if project.is_empty() {
        return Err(Error::Claim("/submods/gce/project_id"));
    }

    let nonce = binding.nonce();
    if !strings(claims, "/eat_nonce")
        .map_err(|_| Error::Nonce)?
        .contains(&nonce.as_str())
    {
        return Err(Error::Nonce);
    }

    Ok(Journal {
        image_digest,
        project_id: project_id(project),
        vaults_root: binding.vaults_root,
        issued_at: iat,
        evm_signer: binding.evm_signer,
        solana_signer: binding.solana_signer,
        key_hash: [0; 32],
    })
}

pub mod pki;

#[cfg(any(test, feature = "fixture"))]
pub mod fixture;

#[cfg(test)]
mod tests;
