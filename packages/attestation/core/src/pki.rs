//! A PKI token's certificate chain.
//!
//! Confidential Space can sign a token with a short-lived key whose
//! certificate travels in the token's header (`x5c`): the leaf certificate,
//! then the intermediate CA that issued it, then Google's Confidential Space
//! Root CA. The chain is checked up to its last certificate, which is trusted
//! by its hash alone ([`root_hash`]): the journal names that hash, and a
//! checker accepts the proof only if it pins it. So Google may rotate its
//! signing keys and intermediates as it likes, and nothing here or on any
//! chain changes until the root does.
//!
//! What a chain must be, each check against the token's `iat`:
//!
//! - two to [`MAX_CHAIN`] X.509 v3 certificates in strict DER, each signed
//!   with sha256WithRSAEncryption, each holding an RSA key of at least
//!   [`crate::MIN_KEY_BITS`] bits;
//! - every certificate, the root included, valid at `iat`;
//! - every certificate but the root signed by the next one's key, and naming
//!   it as its issuer;
//! - every certificate above the leaf a CA: basic constraints with `cA`,
//!   key usage with `keyCertSign`, and a path length constraint, if any, that
//!   allows the CAs below it;
//! - the leaf not a CA, and allowed to sign (`digitalSignature`) if it says
//!   what its key is for;
//! - no critical extension other than those two.
//!
//! The root's own signature is not checked: it is trusted by its hash, which
//! a self-signature would add nothing to. Revocation is not checked.

use alloc::vec::Vec;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rsa::RsaPublicKey;
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::pkcs8::DecodePublicKey;
use rsa::signature::Verifier;
use rsa::traits::PublicKeyParts;
use sha2::{Digest, Sha256};
use x509_cert::Certificate;
use x509_cert::certificate::Version;
use x509_cert::der::asn1::ObjectIdentifier;
use x509_cert::der::{Decode, Encode, Header, Reader, SliceReader, Tagged};
use x509_cert::ext::pkix::{BasicConstraints, KeyUsage};
use x509_cert::spki::AlgorithmIdentifierOwned;

use crate::{Error, MIN_KEY_BITS};

/// The most certificates a chain may hold: a leaf, a root and up to two
/// intermediates between them.
pub const MAX_CHAIN: usize = 4;

/// `sha256WithRSAEncryption` (RFC 4055).
const SHA256_WITH_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
/// `id-ce-basicConstraints`.
const BASIC_CONSTRAINTS: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.19");
/// `id-ce-keyUsage`.
const KEY_USAGE: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.15");

/// What a chain is trusted by: `sha256` of its root certificate's DER, as
/// `openssl x509 -outform der | sha256sum` gives it.
pub fn root_hash(der: &[u8]) -> [u8; 32] {
    Sha256::digest(der).into()
}

/// A PEM certificate's DER bytes.
pub fn pem_der(pem: &str) -> Result<Vec<u8>, Error> {
    let body = pem
        .trim()
        .strip_prefix("-----BEGIN CERTIFICATE-----")
        .and_then(|rest| rest.strip_suffix("-----END CERTIFICATE-----"))
        .ok_or(Error::Malformed("certificate"))?;
    let text: alloc::string::String = body.split_whitespace().collect();
    STANDARD
        .decode(text)
        .map_err(|_| Error::Malformed("certificate"))
}

/// One certificate of the chain: its parsed form, the exact bytes its
/// signature covers, and its key.
struct Link {
    cert: Certificate,
    tbs: Vec<u8>,
    key: RsaPublicKey,
}

fn refused(why: &'static str) -> Error {
    Error::Chain(why)
}

fn sha256_with_rsa(algorithm: &AlgorithmIdentifierOwned) -> bool {
    // The parameters are NULL, or (as some encoders write it) absent.
    algorithm.oid == SHA256_WITH_RSA
        && algorithm
            .parameters
            .as_ref()
            .is_none_or(|any| any.tag() == x509_cert::der::Tag::Null && any.value().is_empty())
}

fn link(der: &[u8]) -> Result<Link, Error> {
    let cert = Certificate::from_der(der).map_err(|_| refused("certificate"))?;
    // The bytes the signature covers, as they are, not as re-encoded: the
    // first element inside the outer sequence the parse above accepted.
    let tbs = {
        let mut reader = SliceReader::new(der).map_err(|_| refused("certificate"))?;
        Header::decode(&mut reader).map_err(|_| refused("certificate"))?;
        reader
            .tlv_bytes()
            .map_err(|_| refused("certificate"))?
            .to_vec()
    };
    let body = &cert.tbs_certificate;
    if body.version != Version::V3 {
        return Err(refused("version"));
    }
    if !sha256_with_rsa(&cert.signature_algorithm) || body.signature != cert.signature_algorithm {
        return Err(refused("algorithm"));
    }
    let spki = body
        .subject_public_key_info
        .to_der()
        .map_err(|_| refused("key"))?;
    let key = RsaPublicKey::from_public_key_der(&spki).map_err(|_| refused("key"))?;
    if key.n().bits() < MIN_KEY_BITS {
        return Err(Error::WeakKey);
    }
    Ok(Link { cert, tbs, key })
}

/// A certificate's validity, inclusive at both ends, in Unix seconds.
fn window(cert: &Certificate) -> (u64, u64) {
    let validity = &cert.tbs_certificate.validity;
    (
        validity.not_before.to_unix_duration().as_secs(),
        validity.not_after.to_unix_duration().as_secs(),
    )
}

fn signed_by(child: &Link, issuer: &RsaPublicKey) -> bool {
    let Some(signature) = child.cert.signature.as_bytes() else {
        return false;
    };
    let Ok(signature) = Signature::try_from(signature) else {
        return false;
    };
    VerifyingKey::<Sha256>::new(issuer.clone())
        .verify(&child.tbs, &signature)
        .is_ok()
}

/// Basic constraints and key usage, refusing any other critical extension.
fn constraints(cert: &Certificate) -> Result<(Option<BasicConstraints>, Option<KeyUsage>), Error> {
    let body = &cert.tbs_certificate;
    for extension in body.extensions.as_deref().unwrap_or(&[]) {
        if extension.critical
            && extension.extn_id != BASIC_CONSTRAINTS
            && extension.extn_id != KEY_USAGE
        {
            return Err(refused("critical extension"));
        }
    }
    // Each at most once (`get` refuses a second), and well formed.
    let basic = body
        .get::<BasicConstraints>()
        .map_err(|_| refused("basic constraints"))?
        .map(|(_, basic)| basic);
    let usage = body
        .get::<KeyUsage>()
        .map_err(|_| refused("key usage"))?
        .map(|(_, usage)| usage);
    Ok((basic, usage))
}

/// The certificates of an `x5c` header, leaf first (standard base64 DER).
pub fn decode_x5c(x5c: &serde_json::Value) -> Result<Vec<Vec<u8>>, Error> {
    let items = x5c.as_array().ok_or(refused("x5c"))?;
    if items.len() < 2 || items.len() > MAX_CHAIN {
        return Err(refused("x5c"));
    }
    items
        .iter()
        .map(|item| {
            let text = item.as_str().ok_or(refused("x5c"))?;
            STANDARD.decode(text).map_err(|_| refused("x5c"))
        })
        .collect()
}

/// A chain whose links, CAs and leaf are as they must be; whether each
/// certificate is valid at the token's time is [`Chain::check_time`].
pub struct Chain {
    /// The leaf's key, which signs the token.
    pub leaf: RsaPublicKey,
    /// The root certificate's hash ([`root_hash`]).
    pub root: [u8; 32],
    /// Each certificate's validity, leaf first.
    windows: Vec<(u64, u64)>,
}

impl Chain {
    /// Every certificate valid at `at`, the token's `iat`.
    pub fn check_time(&self, at: u64) -> Result<(), Error> {
        if self
            .windows
            .iter()
            .all(|(from, until)| *from <= at && at <= *until)
        {
            Ok(())
        } else {
            Err(refused("validity"))
        }
    }
}

/// The checks of the chain that need no time: each link signed by the next,
/// the CAs and the leaf as they must be.
pub fn check_chain(chain: &[Vec<u8>]) -> Result<Chain, Error> {
    if chain.len() < 2 || chain.len() > MAX_CHAIN {
        return Err(refused("x5c"));
    }
    let links: Vec<Link> = chain
        .iter()
        .map(|der| link(der))
        .collect::<Result<_, _>>()?;
    for (at, current) in links.iter().enumerate() {
        let (basic, usage) = constraints(&current.cert)?;
        if at == 0 {
            if basic.is_some_and(|basic| basic.ca) {
                return Err(refused("leaf is a CA"));
            }
            if usage.is_some_and(|usage| !usage.digital_signature()) {
                return Err(refused("key usage"));
            }
        } else {
            let basic = basic.ok_or(refused("not a CA"))?;
            if !basic.ca {
                return Err(refused("not a CA"));
            }
            if !usage.is_some_and(|usage| usage.key_cert_sign()) {
                return Err(refused("key usage"));
            }
            // The CAs below this one, the leaf not counted.
            if basic
                .path_len_constraint
                .is_some_and(|most| at - 1 > usize::from(most))
            {
                return Err(refused("path length"));
            }
        }
        if let Some(issuer) = links.get(at + 1) {
            if current.cert.tbs_certificate.issuer != issuer.cert.tbs_certificate.subject {
                return Err(refused("issuer"));
            }
            if !signed_by(current, &issuer.key) {
                return Err(refused("signature"));
            }
        }
    }
    let windows = links.iter().map(|link| window(&link.cert)).collect();
    let leaf = links
        .into_iter()
        .next()
        .expect("two certificates at least")
        .key;
    Ok(Chain {
        leaf,
        root: root_hash(chain.last().expect("two certificates at least")),
        windows,
    })
}
