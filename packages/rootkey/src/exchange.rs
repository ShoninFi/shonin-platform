//! Passing the root, or a key derived from it, from a holder to another
//! attested enclave.
//!
//! The asking enclave makes an ephemeral X25519 key, and asks the
//! Confidential Space launcher for an attestation token whose nonce binds
//! that key and the purpose it wants ([`exchange_nonce`]). It sends the
//! token, the key's public half and the purpose (and, for a vault's keys, a
//! vault key's proof) to a holder ([`ExchangeRequest`]).
//!
//! The holder ([`authorise`]):
//!
//! 1. checks the token exactly as the attestation guest does, with the
//!    exchange's nonce in place of a blessing's, against the Google keys an
//!    allowed checker on the canonical chain trusts, and refuses one issued
//!    more than [`MAX_TOKEN_AGE`] ago (and accepts one issued up to
//!    [`CLOCK_SKEW`] ahead of its own clock);
//! 2. reads the canonical chain for the image's kind and the project;
//! 3. decides what that kind may have ([`permission`]), and for a vault's
//!    keys checks the vault key's proof and its blessing, read from the
//!    registry on the vault's own chain, where vault keys are blessed;
//! 4. seals the root, or the purpose's private key, to the ephemeral key,
//!    binding the token's hash and the purpose ([`Release`]).
//!
//! A replayed request gets nothing its sender could not already open: the
//! release is sealed to the ephemeral key in the token's nonce, which only
//! the enclave that asked for the token holds.
//!
//! # Reusing the guest's check
//!
//! The guest's [`shonin_attestation_core::verify`] checks the nonce last,
//! after the signature and every other claim. So the token is run through
//! it unchanged, with an empty binding: refused for its nonce (or, by
//! chance, accepted) means every other check passed, and the exchange's own
//! nonce is then checked in the claims whose signature it just verified. The
//! attestation crate is not changed, so neither is the guest nor its image
//! id; this crate's tests pin the order this relies on.

use alloy::primitives::{Address, B256, Bytes, FixedBytes, Signature, U256, keccak256};
use alloy::signers::SignerSync;
use alloy::signers::local::PrivateKeySigner;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hpke::{Deserializable, Kem as _, Serializable};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use shonin_attestation_core::{Binding, Input, Jwk};
use zeroize::Zeroizing;

use crate::Error;
use crate::chain_view::{ChainView, ImageKind};
use crate::purpose::{PLATFORM_SECRETS, Purpose};
use crate::root::{KeyPair, Root};
use crate::sealed::{Kem, PrivateKey, PublicKey, base64_bytes, hpke_open, hpke_seal};

/// What the exchange's nonce hashes first.
pub const NONCE_TAG: &[u8] = b"shonin-root-exchange:v1";
/// What a vault key's proof's digest begins with.
pub const PROOF_TAG: &[u8] = b"shonin:root-exchange-proof:v1";
/// The `info` a release is sealed under.
pub const RELEASE_INFO: &[u8] = b"shonin:root-exchange:v1:release";
/// The version of the release format.
pub const RELEASE_VERSION: u32 = 1;
/// The oldest token an exchange accepts, in seconds since its `iat`. A
/// requester asks for its token just before it asks; the token's own hour
/// is for a prover, which an exchange doesn't wait for.
pub const MAX_TOKEN_AGE: u64 = 10 * 60;

/// How far ahead of the holder's clock a token's `iat` and `nbf` may be:
/// Google's attestation service stamps a token seconds ahead of the
/// machines' own clocks (about 8 seconds, measured on Confidential Space in
/// October 2026), and a requester sends its token at once.
pub const CLOCK_SKEW: u64 = 30;

/// The nonce the asking enclave puts in its token: the lowercase hex of
/// `sha256("shonin-root-exchange:v1" ‖ ephemeral public key ‖ purpose)`.
pub fn exchange_nonce(ephemeral_public: &[u8; 32], purpose: &Purpose) -> String {
    let mut hasher = Sha256::new();
    hasher.update(NONCE_TAG);
    hasher.update(ephemeral_public);
    hasher.update(purpose.to_bytes());
    hex::encode(hasher.finalize())
}

/// A vault key's word that the asking enclave holds it: its signature over
/// the request's ephemeral key and purpose, and the vault and bundle it is
/// blessed for.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct VaultProof {
    pub vault_chain: u64,
    pub vault: Address,
    pub strategy_hash: B256,
    /// 65 bytes, `r ‖ s ‖ v`, as an Ethereum signed message over
    /// [`VaultProof::digest`].
    pub signature: Bytes,
}

impl VaultProof {
    /// `keccak256("shonin:root-exchange-proof:v1" ‖ ephemeral public key ‖
    /// purpose ‖ vaultChain (32-byte word) ‖ vault ‖ strategyHash)`.
    pub fn digest(
        ephemeral_public: &[u8; 32],
        purpose: &Purpose,
        vault_chain: u64,
        vault: Address,
        strategy_hash: B256,
    ) -> B256 {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(PROOF_TAG);
        bytes.extend_from_slice(ephemeral_public);
        bytes.extend_from_slice(&purpose.to_bytes());
        bytes.extend_from_slice(&U256::from(vault_chain).to_be_bytes::<32>());
        bytes.extend_from_slice(vault.as_slice());
        bytes.extend_from_slice(strategy_hash.as_slice());
        keccak256(bytes)
    }

    /// The vault key's proof for a request.
    pub fn sign(
        vault_key: &PrivateKeySigner,
        ephemeral_public: &[u8; 32],
        purpose: &Purpose,
        vault_chain: u64,
        vault: Address,
        strategy_hash: B256,
    ) -> Result<VaultProof, Error> {
        let digest = Self::digest(ephemeral_public, purpose, vault_chain, vault, strategy_hash);
        let signature = vault_key
            .sign_message_sync(digest.as_slice())
            .map_err(|_| Error::ProofInvalid)?;
        Ok(VaultProof {
            vault_chain,
            vault,
            strategy_hash,
            signature: Bytes::from(signature.as_bytes().to_vec()),
        })
    }

    /// The key that signed it for this request.
    pub fn key(&self, ephemeral_public: &[u8; 32], purpose: &Purpose) -> Result<Address, Error> {
        let signature =
            Signature::try_from(self.signature.as_ref()).map_err(|_| Error::ProofInvalid)?;
        let digest = Self::digest(
            ephemeral_public,
            purpose,
            self.vault_chain,
            self.vault,
            self.strategy_hash,
        );
        signature
            .recover_address_from_msg(digest.as_slice())
            .map_err(|_| Error::ProofInvalid)
    }
}

/// What the asking enclave sends a holder (`POST /rootkey/exchange`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ExchangeRequest {
    /// The attestation token, its nonce [`exchange_nonce`] of the two below.
    pub token: String,
    pub ephemeral_public: FixedBytes<32>,
    pub purpose: Purpose,
    /// A vault key's proof, for a vault's keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<VaultProof>,
}

/// What a holder sends back: the root or the purpose's private key, sealed
/// to the request's ephemeral key.
///
/// ```json
/// {
///   "version": 1,
///   "purpose": "platform:dummy",
///   "token_hash": "0x<sha256 of the token>",
///   "enc": "<base64>",
///   "ciphertext": "<base64>"
/// }
/// ```
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: u32,
    pub purpose: Purpose,
    pub token_hash: FixedBytes<32>,
    #[serde(with = "base64_bytes")]
    pub enc: Vec<u8>,
    #[serde(with = "base64_bytes")]
    pub ciphertext: Vec<u8>,
}

fn release_aad(token_hash: &[u8; 32], purpose: &Purpose) -> Vec<u8> {
    [token_hash.as_slice(), &purpose.to_bytes()].concat()
}

fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// What an exchange gave.
#[derive(Debug)]
pub enum Received {
    Root(Root),
    Key(KeyPair),
}

/// The asking side of one exchange: an ephemeral key, made for it alone.
pub struct Requester {
    purpose: Purpose,
    private: PrivateKey,
    public: [u8; 32],
}

impl std::fmt::Debug for Requester {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Requester")
            .field("purpose", &self.purpose.to_string())
            .field(
                "ephemeral_public",
                &format!("0x{}", hex::encode(self.public)),
            )
            .finish()
    }
}

impl Requester {
    /// A fresh ephemeral key, from the operating system's randomness, to
    /// ask for `purpose`.
    pub fn new(purpose: Purpose) -> Result<Requester, Error> {
        if !purpose.is_well_formed() {
            return Err(Error::Malformed("purpose"));
        }
        let mut ikm = Zeroizing::new([0u8; 32]);
        getrandom::fill(ikm.as_mut()).map_err(|_| Error::Randomness)?;
        let (private, public) = Kem::derive_keypair(ikm.as_slice());
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&public.to_bytes());
        Ok(Requester {
            purpose,
            private,
            public: bytes,
        })
    }

    pub fn purpose(&self) -> &Purpose {
        &self.purpose
    }

    pub fn ephemeral_public(&self) -> [u8; 32] {
        self.public
    }

    /// The nonce to ask the launcher's token for.
    pub fn nonce(&self) -> String {
        exchange_nonce(&self.public, &self.purpose)
    }

    /// A vault key's proof for this request.
    pub fn prove(
        &self,
        vault_key: &PrivateKeySigner,
        vault_chain: u64,
        vault: Address,
        strategy_hash: B256,
    ) -> Result<VaultProof, Error> {
        VaultProof::sign(
            vault_key,
            &self.public,
            &self.purpose,
            vault_chain,
            vault,
            strategy_hash,
        )
    }

    /// The request to send, with the token the launcher gave for
    /// [`Requester::nonce`].
    pub fn request(&self, token: String, proof: Option<VaultProof>) -> ExchangeRequest {
        ExchangeRequest {
            token,
            ephemeral_public: FixedBytes(self.public),
            purpose: self.purpose.clone(),
            proof,
        }
    }

    /// Open what a holder released for `request`.
    pub fn open(&self, request: &ExchangeRequest, release: &Release) -> Result<Received, Error> {
        if release.version != RELEASE_VERSION {
            return Err(Error::Malformed("release's version"));
        }
        let hash = token_hash(&request.token);
        if release.purpose != self.purpose
            || request.purpose != self.purpose
            || request.ephemeral_public.0 != self.public
            || release.token_hash.0 != hash
        {
            return Err(Error::Open);
        }
        let secret = hpke_open(
            &self.private,
            &release.enc,
            RELEASE_INFO,
            &release_aad(&hash, &self.purpose),
            &release.ciphertext,
        )?;
        match self.purpose {
            Purpose::Root => {
                let bytes: [u8; 32] = secret
                    .as_slice()
                    .try_into()
                    .map_err(|_| Error::Malformed("released root"))?;
                Ok(Received::Root(Root::from_bytes(Zeroizing::new(bytes))))
            }
            _ => KeyPair::from_private_bytes(self.purpose.clone(), &secret).map(Received::Key),
        }
    }
}

/// What a kind of image may be given of a purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Never,
    Granted,
    /// Only with a vault key's proof, for a vault (and bundle) it is blessed
    /// for.
    WithVaultProof,
}

/// What each kind may have:
///
/// | kind | may have |
/// | --- | --- |
/// | holder | the root |
/// | executor | the root; a vault's venue keys and its bundle's key, with that vault's key's proof |
/// | unwinder | a vault's venue keys, with that vault's key's proof |
/// | builder | the builder key |
/// | reviewer | the platform secrets `reviewer-model` and `dummy` |
pub fn permission(kind: ImageKind, purpose: &Purpose) -> Permission {
    use ImageKind::*;
    match (kind, purpose) {
        (Holder | Executor, Purpose::Root) => Permission::Granted,
        (Executor | Unwinder, Purpose::Venue { .. }) => Permission::WithVaultProof,
        (Executor, Purpose::Bundle(_)) => Permission::WithVaultProof,
        (Builder, Purpose::Builder) => Permission::Granted,
        (Reviewer, Purpose::Platform(name)) if PLATFORM_SECRETS.contains(&name.as_str()) => {
            Permission::Granted
        }
        _ => Permission::Never,
    }
}

/// What a token that passed every check attests to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attested {
    pub image_digest: B256,
    /// `keccak256` of the GCE project id, as the registry lists projects.
    pub project_id: B256,
    pub issued_at: u64,
    /// The hash of the Google key that signed it.
    pub key_hash: [u8; 32],
}

fn segment(token: &str, index: usize) -> Result<Value, Error> {
    let part = token
        .split('.')
        .nth(index)
        .ok_or(Error::Malformed("token"))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| Error::Malformed("token"))?;
    serde_json::from_slice(&bytes).map_err(|_| Error::Malformed("token"))
}

/// Check a token as the attestation guest does, with `nonce` in place of a
/// blessing's, against those of `google_keys` whose hash is in `trusted`.
///
/// `google_keys` is what the caller fetched from Google's JWKS; `trusted` is
/// what the canonical chain's allowed checkers trust
/// ([`ChainView::trusted_google_keys`]). A key Google publishes that no
/// checker trusts signs nothing here.
pub fn check_token(
    token: &str,
    google_keys: &[Jwk],
    trusted: &[[u8; 32]],
    now: u64,
    nonce: &str,
) -> Result<Attested, Error> {
    use shonin_attestation_core::Error as Guest;

    let keys: Vec<Jwk> = google_keys
        .iter()
        .filter(|key| trusted.contains(&key.key_hash()))
        .cloned()
        .collect();
    // Checked as of a moment `CLOCK_SKEW` ahead, so a token stamped by a
    // clock a little ahead of ours passes; its age is judged at `now`.
    let input = Input {
        token: token.into(),
        keys: keys.clone(),
        now: now.saturating_add(CLOCK_SKEW),
        binding: Binding::default(),
    };
    match shonin_attestation_core::verify(&input) {
        // The guest checks the nonce last: every other check passed.
        Ok(_) | Err(Guest::Nonce) => {}
        Err(Guest::UnknownKey) => {
            let kid = segment(token, 0)?
                .get("kid")
                .and_then(Value::as_str)
                .map(str::to_string);
            let published = google_keys
                .iter()
                .any(|key| Some(key.kid.as_str()) == kid.as_deref());
            return Err(if published {
                Error::UntrustedGoogleKey
            } else {
                Error::Token(Guest::UnknownKey)
            });
        }
        Err(other) => return Err(Error::Token(other)),
    }

    // The signature has verified: the claims are Google's.
    let kid = segment(token, 0)?
        .get("kid")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or(Error::Malformed("token"))?;
    let key_hash = keys
        .iter()
        .find(|key| key.kid == kid)
        .map(Jwk::key_hash)
        .ok_or(Error::UntrustedGoogleKey)?;
    let claims = segment(token, 1)?;
    let nonces: Vec<&str> = match claims.get("eat_nonce") {
        Some(Value::String(one)) => vec![one.as_str()],
        Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !nonces.contains(&nonce) {
        return Err(Error::Nonce);
    }
    let issued_at = claims
        .get("iat")
        .and_then(Value::as_u64)
        .ok_or(Error::Malformed("token"))?;
    if now.saturating_sub(issued_at) > MAX_TOKEN_AGE {
        return Err(Error::Stale);
    }
    let image_digest = claims
        .pointer("/submods/container/image_digest")
        .and_then(Value::as_str)
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .and_then(|hex| hex.parse::<B256>().ok())
        .ok_or(Error::Malformed("token"))?;
    let project = claims
        .pointer("/submods/gce/project_id")
        .and_then(Value::as_str)
        .ok_or(Error::Malformed("token"))?;
    Ok(Attested {
        image_digest,
        project_id: B256::from(shonin_attestation_core::project_id(project)),
        issued_at,
        key_hash,
    })
}

fn chain_unavailable(error: anyhow::Error) -> Error {
    tracing::warn!(error = format!("{error:#}"), "reading the chain");
    Error::ChainUnavailable
}

/// Decide a request, and release what it may have, sealed to its ephemeral
/// key.
///
/// `google_keys` are Google's signing keys as the caller last fetched them
/// (only those an allowed checker trusts count); `now` is the holder's
/// clock, in seconds. The canonical chain is read at one block
/// ([`ChainView::pin`]).
pub async fn authorise<V: ChainView>(
    root: &Root,
    google_keys: &[Jwk],
    request: &ExchangeRequest,
    now: u64,
    view: &V,
) -> Result<Release, Error> {
    let at = view.pin(view.chain_id()).await.map_err(chain_unavailable)?;
    authorise_at(root, google_keys, request, now, view, at).await
}

/// [`authorise`], with every read of the canonical chain at block `at`, one
/// [`ChainView::pin`] gave; a vault's own chain, when it is another, is
/// pinned once for the decision too.
pub async fn authorise_at<V: ChainView>(
    root: &Root,
    google_keys: &[Jwk],
    request: &ExchangeRequest,
    now: u64,
    view: &V,
    at: u64,
) -> Result<Release, Error> {
    let ephemeral = request.ephemeral_public.0;
    PublicKey::from_bytes(&ephemeral).map_err(|_| Error::Malformed("ephemeral key"))?;
    let purpose = &request.purpose;
    if !purpose.is_well_formed() {
        return Err(Error::Malformed("purpose"));
    }

    let nonce = exchange_nonce(&ephemeral, purpose);
    let trusted = view
        .trusted_google_keys(at)
        .await
        .map_err(chain_unavailable)?;
    let attested = check_token(&request.token, google_keys, &trusted, now, &nonce)?;

    let kind = view
        .image_kind(attested.image_digest, at)
        .await
        .map_err(chain_unavailable)?
        .ok_or(Error::ImageNotAllowed)?;
    if !view
        .project_allowed(attested.project_id, at)
        .await
        .map_err(chain_unavailable)?
    {
        return Err(Error::ProjectNotAllowed);
    }

    match permission(kind, purpose) {
        Permission::Never => {
            return Err(Error::NotForKind {
                kind,
                purpose: purpose.clone(),
            });
        }
        Permission::Granted => {}
        Permission::WithVaultProof => check_proof(request, &attested, view, at).await?,
    }

    let secret: Zeroizing<Vec<u8>> = match purpose {
        Purpose::Root => Zeroizing::new(root.expose().to_vec()),
        _ => root.key_pair(purpose)?.private_bytes(),
    };
    let hash = token_hash(&request.token);
    let (enc, ciphertext) = hpke_seal(
        &ephemeral,
        RELEASE_INFO,
        &release_aad(&hash, purpose),
        &secret,
    )?;
    Ok(Release {
        version: RELEASE_VERSION,
        purpose: purpose.clone(),
        token_hash: FixedBytes(hash),
        enc,
        ciphertext,
    })
}

/// A vault's keys go only to an enclave that holds a key blessed for that
/// vault (and, for a bundle's key, that bundle), from the asker's own image.
///
/// A vault key is blessed on the vault's own chain, the chain the proof
/// (and a venue's purpose) names, so the blessing and the key's image are
/// read from that chain's registry; a chain whose registry isn't read here
/// is refused, never answered from another. The asker's image and project
/// were judged on the canonical chain, at block `canonical_at`; the vault's
/// chain, when it is another, is pinned to one block for both its reads.
async fn check_proof<V: ChainView>(
    request: &ExchangeRequest,
    attested: &Attested,
    view: &V,
    canonical_at: u64,
) -> Result<(), Error> {
    let purpose = &request.purpose;
    let proof = request
        .proof
        .as_ref()
        .ok_or_else(|| Error::ProofMissing(purpose.clone()))?;
    let matches = match purpose {
        Purpose::Venue {
            vault_chain, vault, ..
        } => proof.vault_chain == *vault_chain && proof.vault == *vault,
        Purpose::Bundle(hash) => proof.strategy_hash == *hash,
        _ => false,
    };
    if !matches {
        return Err(Error::ProofMismatch);
    }
    let key = proof.key(&request.ephemeral_public.0, purpose)?;
    if !view.reads_chain(proof.vault_chain) {
        return Err(Error::VaultChainNotRead(proof.vault_chain));
    }
    let at = if proof.vault_chain == view.chain_id() {
        canonical_at
    } else {
        view.pin(proof.vault_chain)
            .await
            .map_err(chain_unavailable)?
    };
    let blessed = view
        .is_blessed_for(key, proof.vault_chain, proof.vault, proof.strategy_hash, at)
        .await
        .map_err(chain_unavailable)?;
    let image = view
        .key_image(proof.vault_chain, key, at)
        .await
        .map_err(chain_unavailable)?;
    if !blessed || image != Some(attested.image_digest) {
        return Err(Error::NotBlessed);
    }
    Ok(())
}
