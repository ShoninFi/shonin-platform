//! An attested service's own keys: a builder's or a reviewer's.
//!
//! A service asks the holder for its seed by exchange ([`identity`]): the
//! holder derives it from the root and the image digest, project and kind
//! the service's token attests ([`crate::Root::service_seed`]), so every
//! boot of the same image under the same root has the same seed, and so the
//! same keys, blessings and gas. From the seed come:
//!
//! - the **signer**, a secp256k1 key the registry blesses as the service's
//!   kind, which sends the service's transactions and signs its requests to
//!   the platform's internal routes ([`request_digest`]);
//! - the **state key**, which seals what the service keeps off its machine
//!   ([`Identity::seal_state`]): a blob sealed under one image's seed opens
//!   under no other.
//!
//! [`ServiceAttestation`] is the service's status port for its blessings, in
//! the executor's shape: one leaf per chain served, naming no vault.
//!
//! # Encodings
//!
//! | what | how |
//! | --- | --- |
//! | a derived secret | HKDF-SHA256, salt `shonin:service:v1`, ikm the seed, info a label below |
//! | the signer | info `shonin:service:v1:signer` ‖ a counter byte (0, then 1, ... until the 32 bytes are a valid scalar) |
//! | the state key | info `shonin:service:v1:state`, 32 bytes |
//! | the state key's id | info `shonin:service:v1:state-id`, 16 bytes |
//! | a sealed blob | `01` ‖ state key's id (16) ‖ nonce (24) ‖ XChaCha20-Poly1305 ciphertext and tag; aad `01` ‖ state key's id ‖ the blob's name |
//! | a request's digest | `keccak256("shonin:internal-request:v1" ‖ method ‖ 00 ‖ path ‖ 00 ‖ sha256(body) ‖ at (8 bytes, big-endian))`, signed as an Ethereum signed message (EIP-191), 65 bytes |

use std::fmt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, B256, Signature, keccak256};
use alloy::signers::SignerSync;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, bail};
use chacha20poly1305::aead::Aead;
use chacha20poly1305::aead::Payload;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::exchange::{Received, Release, Requester};
use crate::purpose::Purpose;
use crate::{Error, launcher};

mod attestation;

pub use attestation::{BindingView, LeafView, Mode, Refusal, SealPost, ServiceAttestation, Status};

/// The salt every derivation from a service's seed is extracted under.
pub const SALT: &[u8] = b"shonin:service:v1";
/// The label the signer is expanded under, before its counter byte.
pub const SIGNER_LABEL: &[u8] = b"shonin:service:v1:signer";
/// The label the state key is expanded under.
pub const STATE_LABEL: &[u8] = b"shonin:service:v1:state";
/// The label the state key's id is expanded under.
pub const STATE_ID_LABEL: &[u8] = b"shonin:service:v1:state-id";
/// The version byte a sealed blob begins with.
pub const STATE_VERSION: u8 = 1;
/// What a request's digest begins with.
pub const REQUEST_TAG: &[u8] = b"shonin:internal-request:v1";

/// The header a signed request carries its time in: unix seconds.
pub const AT_HEADER: &str = "x-shonin-at";
/// The header a signed request carries its signature in: `0x` and 65 bytes
/// of hex, over [`request_digest`].
pub const SIGNATURE_HEADER: &str = "x-shonin-signature";

const STATE_ID_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = 1 + STATE_ID_LEN;

/// Why a sealed blob did not open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StateError {
    /// It was sealed under another seed: another image, project, kind or
    /// root. It is not this service's to read, and is best set aside.
    #[error("the blob was sealed under another service's key")]
    OtherKey,
    /// It names this service's key but is malformed, was sealed under
    /// another name, or was altered.
    #[error("the blob is malformed, sealed under another name, or altered")]
    Corrupt,
}

/// A service's keys, derived from its seed. Its [`fmt::Debug`] says only the
/// signer's address; the seed itself is not kept.
pub struct Identity {
    signer: PrivateKeySigner,
    state_key: Zeroizing<[u8; 32]>,
    state_id: [u8; STATE_ID_LEN],
}

fn expand(seed: &[u8; 32], info: &[&[u8]], out: &mut [u8]) {
    let info: Vec<u8> = info.concat();
    Hkdf::<Sha256>::new(Some(SALT), seed)
        .expand(&info, out)
        .expect("a 32-byte output is within HKDF-SHA256's limit");
}

impl Identity {
    fn derive(seed: &[u8; 32]) -> Identity {
        let signer = (0..=u8::MAX)
            .find_map(|counter| {
                let mut secret = Zeroizing::new([0u8; 32]);
                expand(seed, &[SIGNER_LABEL, &[counter]], secret.as_mut());
                // Zero or past the curve's order is no key; the next counter
                // is the answer.
                PrivateKeySigner::from_bytes(&B256::from(*secret)).ok()
            })
            .expect("256 draws in a row outside the curve's order");
        let mut state_key = Zeroizing::new([0u8; 32]);
        expand(seed, &[STATE_LABEL], state_key.as_mut());
        let mut state_id = [0u8; STATE_ID_LEN];
        expand(seed, &[STATE_ID_LABEL], &mut state_id);
        Identity {
            signer,
            state_key,
            state_id,
        }
    }

    /// The identity a released seed derives.
    pub fn from_received(received: Received) -> Result<Identity, Error> {
        match received {
            Received::Seed(seed) => Ok(Identity::derive(&seed)),
            _ => Err(Error::Malformed("release: not a service's seed")),
        }
    }

    /// The identity of a fixed seed, for tests.
    #[cfg(any(test, feature = "test-identity"))]
    pub fn from_seed(seed: [u8; 32]) -> Identity {
        Identity::derive(&Zeroizing::new(seed))
    }

    /// The signer's address: the key the registry blesses.
    pub fn address(&self) -> Address {
        self.signer.address()
    }

    /// The signer, to send transactions with (`Client::with_key`).
    pub fn signer(&self) -> PrivateKeySigner {
        self.signer.clone()
    }

    /// Sign a request to an internal route: the value of
    /// [`SIGNATURE_HEADER`], `0x` and 65 bytes of hex.
    pub fn sign_request(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        at: u64,
    ) -> Result<String, Error> {
        let signature = self
            .signer
            .sign_message_sync(request_digest(method, path, body, at).as_slice())
            .map_err(|_| Error::Seal)?;
        Ok(alloy::hex::encode_prefixed(signature.as_bytes()))
    }

    fn header(&self) -> [u8; HEADER_LEN] {
        let mut header = [0u8; HEADER_LEN];
        header[0] = STATE_VERSION;
        header[1..].copy_from_slice(&self.state_id);
        header
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(&chacha20poly1305::Key::from(*self.state_key))
    }

    /// Seal `plain` to be kept as the blob `name`: it opens only under this
    /// identity and that name.
    pub fn seal_state(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>, Error> {
        let header = self.header();
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| Error::Randomness)?;
        let aad = [header.as_slice(), name.as_bytes()].concat();
        let ciphertext = self
            .cipher()
            .encrypt(
                &chacha20poly1305::XNonce::from(nonce),
                Payload {
                    msg: plain,
                    aad: &aad,
                },
            )
            .map_err(|_| Error::Seal)?;
        Ok([header.as_slice(), &nonce, &ciphertext].concat())
    }

    /// Open the blob `name`.
    pub fn open_state(&self, name: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, StateError> {
        if sealed.len() < HEADER_LEN + NONCE_LEN + TAG_LEN || sealed[0] != STATE_VERSION {
            return Err(StateError::Corrupt);
        }
        let header = self.header();
        if sealed[..HEADER_LEN] != header {
            return Err(StateError::OtherKey);
        }
        let nonce: [u8; NONCE_LEN] = sealed[HEADER_LEN..HEADER_LEN + NONCE_LEN]
            .try_into()
            .map_err(|_| StateError::Corrupt)?;
        let aad = [header.as_slice(), name.as_bytes()].concat();
        self.cipher()
            .decrypt(
                &chacha20poly1305::XNonce::from(nonce),
                Payload {
                    msg: &sealed[HEADER_LEN + NONCE_LEN..],
                    aad: &aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| StateError::Corrupt)
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Identity({})", self.address())
    }
}

/// What a request to an internal route is signed over:
/// `keccak256("shonin:internal-request:v1" ‖ method ‖ 00 ‖ path ‖ 00 ‖
/// sha256(body) ‖ at)`, `at` 8 bytes big-endian.
pub fn request_digest(method: &str, path: &str, body: &[u8], at: u64) -> B256 {
    let mut bytes = Vec::with_capacity(REQUEST_TAG.len() + method.len() + path.len() + 50);
    bytes.extend_from_slice(REQUEST_TAG);
    bytes.extend_from_slice(method.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(path.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&Sha256::digest(body));
    bytes.extend_from_slice(&at.to_be_bytes());
    keccak256(bytes)
}

/// Who signed a request: the address `signature` (65 bytes, `r ‖ s ‖ v`)
/// recovers to over [`request_digest`]. The caller checks that address is
/// blessed as the kind it expects, and that `at` is recent.
pub fn request_signer(
    method: &str,
    path: &str,
    body: &[u8],
    at: u64,
    signature: &[u8],
) -> Result<Address, Error> {
    let signature =
        Signature::try_from(signature).map_err(|_| Error::Malformed("request signature"))?;
    signature
        .recover_address_from_msg(request_digest(method, path, body, at).as_slice())
        .map_err(|_| Error::Malformed("request signature"))
}

// Asking the holder --------------------------------------------------------

/// How many times [`obtain`] asks before it gives up.
pub const OBTAIN_ATTEMPTS: u32 = 20;
/// The longest an exchange's release may be.
const MOST_RELEASE_BYTES: usize = 64 * 1024;

/// How long to wait after the `tried`th failure: 5 seconds, doubling, at
/// most a minute, so the attempts span about a quarter of an hour, enough
/// for a holder that is restarting.
fn backoff(tried: u32) -> Duration {
    Duration::from_secs((5u64 << tried.saturating_sub(1).min(4)).min(60))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// A token's `iat`, `nbf` and `exp`, read without checking it (it is our own
/// and only logged or waited on).
pub fn token_times(token: &str) -> Option<(Option<u64>, Option<u64>, Option<u64>)> {
    use base64::Engine as _;
    let claims = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(claims.trim_end_matches('='))
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let time = |name: &str| value.get(name).and_then(serde_json::Value::as_u64);
    Some((time("iat"), time("nbf"), time("exp")))
}

/// One exchange: a token from the launcher at `socket` binding a fresh
/// ephemeral key and `purpose`, posted to `holder`'s `/rootkey/exchange`,
/// and the release opened.
pub async fn exchange_once(
    http: &reqwest::Client,
    holder: &str,
    socket: &Path,
    purpose: &Purpose,
) -> Result<Received> {
    let requester = Requester::new(purpose.clone())?;
    let token = launcher::request_token(socket, &requester.nonce()).await?;
    // A token is stamped by a clock seconds ahead of this machine's (and a
    // holder's): wait until it is no longer in the future here, so a holder
    // that allows no skew takes it too.
    if let Some((Some(issued_at), _, _)) = token_times(&token) {
        let ahead = issued_at.saturating_sub(unix_now()).min(60);
        if ahead > 0 {
            tokio::time::sleep(Duration::from_secs(ahead + 1)).await;
        }
    }
    let request = requester.request(token, None);
    let url = format!("{}/rootkey/exchange", holder.trim_end_matches('/'));
    let answer = http
        .post(&url)
        .timeout(Duration::from_secs(60))
        .json(&request)
        .send()
        .await
        .context("posting the exchange request")?;
    let status = answer.status();
    if !status.is_success() {
        let why = crate::body::read_at_most(answer, 4096)
            .await
            .map(|body| String::from_utf8_lossy(&body).into_owned())
            .unwrap_or_default();
        // The token's times beside this machine's clock: a refusal over
        // freshness is a matter of clocks, and this says whose.
        let times = token_times(&request.token);
        tracing::warn!(
            ?times,
            now = unix_now(),
            "the refused token's iat, nbf and exp, and this machine's clock"
        );
        bail!("the holder refused ({status}): {why}");
    }
    let body = crate::body::read_at_most(answer, MOST_RELEASE_BYTES)
        .await
        .context("reading the holder's release")?;
    let release: Release =
        serde_json::from_slice(&body).context("the holder's answer is not a release")?;
    Ok(requester.open(&request, &release)?)
}

/// Ask `holder` for `purpose` by exchange, with a token from the launcher at
/// `socket`, until it is given or [`OBTAIN_ATTEMPTS`] have failed.
pub async fn obtain(holder: &str, socket: &Path, purpose: Purpose) -> Result<Received> {
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("making the HTTP client")?;
    let mut last = None;
    for tried in 1..=OBTAIN_ATTEMPTS {
        match exchange_once(&http, holder, socket, &purpose).await {
            Ok(received) => {
                tracing::info!(%purpose, "obtained by exchange");
                return Ok(received);
            }
            Err(error) => {
                tracing::warn!(%purpose, tried, error = format!("{error:#}"), "the exchange failed");
                last = Some(error);
                if tried < OBTAIN_ATTEMPTS {
                    tokio::time::sleep(backoff(tried)).await;
                }
            }
        }
    }
    Err(last
        .unwrap_or_else(|| anyhow::anyhow!("no attempt was made"))
        .context(format!("obtaining {purpose} from {holder}")))
}

/// This service's identity: its seed, asked of `holder` by exchange.
pub async fn identity(holder: &str, socket: &Path) -> Result<Identity> {
    let received = obtain(holder, socket, Purpose::Service).await?;
    Ok(Identity::from_received(received)?)
}

#[cfg(test)]
mod tests;
