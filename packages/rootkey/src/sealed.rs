//! Sealing to a purpose's published key, and opening with its key pair.
//!
//! HPKE (RFC 9180) in base mode: DHKEM(X25519, HKDF-SHA256), HKDF-SHA256,
//! ChaCha20-Poly1305. The purpose is bound twice: into the key schedule
//! through `info`, and into the AEAD through the aad, so a message sealed for
//! one purpose opens under no other, whatever key it is tried with.
//!
//! # The format, version 1
//!
//! ```json
//! {
//!   "version": 1,
//!   "suite": "DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, ChaCha20-Poly1305",
//!   "purpose": "platform:dummy",
//!   "public_key": "0x<the recipient's 32-byte public key>",
//!   "enc": "<base64 of the 32-byte encapsulated key>",
//!   "ciphertext": "<base64 of the ciphertext and its 16-byte tag>"
//! }
//! ```
//!
//! `info` is `shonin:sealed:v1` ‖ the purpose's bytes; the aad is the
//! purpose's bytes' length (2 bytes, big-endian) ‖ the purpose's bytes ‖ the
//! caller's aad, which is not stored and must be given again to open.

use alloy::primitives::FixedBytes;
use hpke::{Deserializable, OpModeR, OpModeS, Serializable};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::Error;
use crate::published::PublishedKey;
use crate::purpose::Purpose;
use crate::root::KeyPair;

pub(crate) type Kem = hpke::kem::X25519HkdfSha256;
pub(crate) type Kdf = hpke::kdf::HkdfSha256;
pub(crate) type Aead = hpke::aead::ChaCha20Poly1305;
pub(crate) type PrivateKey = <Kem as hpke::Kem>::PrivateKey;
pub(crate) type PublicKey = <Kem as hpke::Kem>::PublicKey;
type EncappedKey = <Kem as hpke::Kem>::EncappedKey;

/// The version of the format this crate writes and reads.
pub const VERSION: u32 = 1;
/// The suite, by RFC 9180's names.
pub const SUITE: &str = "DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, ChaCha20-Poly1305";
/// What `info` begins with for a message sealed to a purpose.
pub const SEALED_INFO: &[u8] = b"shonin:sealed:v1";

/// A message sealed to one purpose's key.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Sealed {
    pub version: u32,
    pub suite: String,
    pub purpose: Purpose,
    /// The key it was sealed to, so the opener can tell it has the right one.
    pub public_key: FixedBytes<32>,
    #[serde(with = "base64_bytes")]
    pub enc: Vec<u8>,
    #[serde(with = "base64_bytes")]
    pub ciphertext: Vec<u8>,
}

/// HPKE's single-shot seal to `public`.
pub(crate) fn hpke_seal(
    public: &[u8; 32],
    info: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let public = PublicKey::from_bytes(public).map_err(|_| Error::Malformed("public key"))?;
    let (enc, ciphertext) =
        hpke::single_shot_seal::<Aead, Kdf, Kem>(&OpModeS::Base, &public, info, plaintext, aad)
            .map_err(|_| Error::Seal)?;
    Ok((enc.to_bytes().to_vec(), ciphertext))
}

/// HPKE's single-shot open with `private`.
pub(crate) fn hpke_open(
    private: &PrivateKey,
    enc: &[u8],
    info: &[u8],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, Error> {
    let enc = EncappedKey::from_bytes(enc).map_err(|_| Error::Open)?;
    hpke::single_shot_open::<Aead, Kdf, Kem>(&OpModeR::Base, private, &enc, info, ciphertext, aad)
        .map(Zeroizing::new)
        .map_err(|_| Error::Open)
}

fn info(purpose: &Purpose) -> Vec<u8> {
    [SEALED_INFO, &purpose.to_bytes()].concat()
}

fn aad(purpose: &Purpose, caller: &[u8]) -> Vec<u8> {
    let bytes = purpose.to_bytes();
    let mut out = Vec::with_capacity(2 + bytes.len() + caller.len());
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(&bytes);
    out.extend_from_slice(caller);
    out
}

/// Seal `plaintext` to a published key, binding `aad`.
///
/// It checks nothing about the key: the caller verifies the published key
/// against the canonical chain's root signer first
/// ([`PublishedKey::verify`]).
pub fn seal(published: &PublishedKey, plaintext: &[u8], aad_: &[u8]) -> Result<Sealed, Error> {
    if published.purpose == Purpose::Root {
        return Err(Error::RootHasNoKeyPair);
    }
    let public: [u8; 32] = published.public_key.0;
    let (enc, ciphertext) = hpke_seal(
        &public,
        &info(&published.purpose),
        &aad(&published.purpose, aad_),
        plaintext,
    )?;
    Ok(Sealed {
        version: VERSION,
        suite: SUITE.into(),
        purpose: published.purpose.clone(),
        public_key: published.public_key,
        enc,
        ciphertext,
    })
}

/// Open a sealed message with its purpose's key pair and the aad it was
/// sealed with.
pub fn open(pair: &KeyPair, sealed: &Sealed, aad_: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
    if sealed.version != VERSION || sealed.suite != SUITE {
        return Err(Error::Malformed("sealed message's version"));
    }
    if sealed.purpose != *pair.purpose() || sealed.public_key.0 != pair.public_key() {
        return Err(Error::Open);
    }
    hpke_open(
        pair.private(),
        &sealed.enc,
        &info(&sealed.purpose),
        &aad(&sealed.purpose, aad_),
        &sealed.ciphertext,
    )
}

/// Bytes as standard base64, padded.
pub(crate) mod base64_bytes {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}
