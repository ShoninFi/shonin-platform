//! The instance's attestation: the tree of every key it holds, the token
//! that binds that tree, and the seal that proves the token.
//!
//! Every vault taken on, on every EVM chain served, is one leaf naming the
//! vault's own key (`chain::attest::key_leaf`). The instance binds its own
//! key and the root over those leaves into the nonce of the token it asks
//! Confidential Space for, so one proof of that token blesses every key in
//! the tree, each with its own Merkle proof.
//!
//! The executor never blesses anything itself and holds no account that
//! could: it publishes the tree, the token and, once a prover has posted
//! one, the seal on its status port (`GET /attestation`), and the keeper
//! sends each `bless` and stocks each blessed key with gas. In development
//! mode, on a chain with the development checker, the executor seals its
//! journal with the attestor role instead of asking for a token, and the
//! keeper blesses from that seal the same way.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::sol_types::SolValue;
use anyhow::{Context, Result, anyhow, bail};
use axum::http::StatusCode;
use base64::Engine;
use chain::bindings::attestation::IAttestationRegistry;
use chain::{Client, Deployments};
use serde::{Deserialize, Serialize};
use shonin_attestation_core::Binding;

use crate::keys::Key;

/// The audience the executor asks its token for, and the only one the
/// attestation guest accepts.
pub const AUDIENCE: &str = shonin_attestation_core::AUDIENCE;

/// Where Confidential Space's launcher answers a workload's token requests.
pub const LAUNCHER_SOCKET: &str = "/run/container_launcher/teeserver.sock";

/// A token is good for an hour, and a proof must be made within it: one
/// older than this, while a seal is still wanted, is asked for again so a
/// prover that starts late still has time to prove it.
const TOKEN_REFRESH: Duration = Duration::from_secs(40 * 60);

/// A change to the tree is acted on once it has stood this long, so vaults
/// taken on together share one token.
const SETTLE: Duration = if cfg!(test) {
    Duration::from_millis(50)
} else {
    Duration::from_secs(5)
};

/// The blessing TTL assumed until a chain has said its own.
const DEFAULT_TTL: u64 = 72 * 3600;

/// How far before the chain's head a development seal is dated, as
/// `chain::attest` does: a public node a block behind would see it as from
/// the future.
const DATED_BEFORE_THE_HEAD: u64 = 60;

/// How the instance attests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// In Confidential Space: a token from the launcher, proved by a prover.
    Launcher,
    /// On a development chain: the journal sealed with the attestor role,
    /// which the development checker trusts.
    Development,
}

/// One key the instance holds, for one vault on one chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leaf {
    /// The chain the blessing is sent on.
    pub chain_id: u64,
    pub vault_chain: u64,
    pub vault: Address,
    pub strategy_hash: B256,
    /// The key the leaf blesses: the vault's own, made in the enclave.
    pub key: Address,
    /// Whether the key wants blessing: not blessed for this vault and
    /// bundle, or down to a third of its blessing.
    pub due: bool,
}

impl Leaf {
    pub fn hash(&self) -> B256 {
        chain::attest::key_leaf(
            U256::from(self.vault_chain),
            self.vault,
            self.strategy_hash,
            U256::from(self.chain_id),
            self.key,
        )
    }
}

/// A token the launcher gave, for the nonce it binds.
#[derive(Clone, Debug)]
struct Held {
    token: String,
    nonce: String,
    issued_at: u64,
    /// The image and the project the token names, so a journal posted for it
    /// can be checked against them.
    image_digest: B256,
    project_id: B256,
}

/// A seal accepted for a nonce, and the journal it proves.
#[derive(Clone, Debug)]
struct Sealed {
    nonce: String,
    seal: Bytes,
    journal: Bytes,
    issued_at: u64,
}

#[derive(Default)]
struct Inner {
    /// By chain and vault, so the tree's order is the same at every look.
    leaves: BTreeMap<(u64, Address), Leaf>,
    token: Option<Held>,
    sealed: Option<Sealed>,
    /// The shortest blessing TTL among the chains served.
    ttl: Option<u64>,
    /// When the tree last changed.
    changed: Option<Instant>,
}

/// The instance's attestation, shared by every chain's sweep and the status
/// port.
#[derive(Clone)]
pub struct Attestation {
    mode: Mode,
    /// The key the journal names. It identifies the instance and signs
    /// nothing.
    instance: Arc<Key>,
    inner: Arc<Mutex<Inner>>,
    changed: Arc<tokio::sync::Notify>,
    /// The chains served, for simulating a `bless` and for sealing in
    /// development mode.
    chains: Arc<Mutex<BTreeMap<u64, (Client, Deployments)>>>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// The tree's root over `leaves` and each leaf's proof, in order.
fn tree(leaves: &[Leaf]) -> (B256, Vec<Vec<B256>>) {
    let hashes: Vec<B256> = leaves.iter().map(Leaf::hash).collect();
    let root = chain::attest::merkle_root(&hashes);
    let proofs = (0..hashes.len())
        .map(|index| chain::attest::merkle_proof(&hashes, index))
        .collect();
    (root, proofs)
}

/// Whether a blessing that ends at `until` wants renewing at `now`: not
/// blessed at all, or a third of `ttl` or less left.
pub fn due(blessed: bool, until: u64, now: u64, ttl: u64) -> bool {
    !blessed || until.saturating_sub(now).saturating_mul(3) <= ttl
}

/// Whether a seal dated `issued_at` blesses for more than a third of `ttl`
/// past `now`, so a key blessed from it is not due again at once.
fn still_gives_a_blessing(issued_at: u64, now: u64, ttl: u64) -> bool {
    issued_at.saturating_add(ttl.saturating_mul(2) / 3) > now
}

/// Whether to ask the launcher for a token now: some key wants blessing, and
/// neither the token held for the current tree, waiting for its seal, nor
/// the seal accepted for it answers that. A token waiting for its seal is
/// asked for again once it is [`TOKEN_REFRESH`] old, so a prover still has
/// time to prove within its hour.
fn token_wanted(
    any_due: bool,
    nonce: &str,
    token: Option<(&str, u64)>,
    sealed: Option<(&str, u64)>,
    now: u64,
    ttl: u64,
) -> bool {
    if !any_due {
        return false;
    }
    let sealed = sealed.filter(|(for_nonce, _)| *for_nonce == nonce);
    let token = token.filter(|(for_nonce, _)| *for_nonce == nonce);
    match (token, sealed) {
        // A seal at least as fresh as the token: a new token is wanted only
        // once that seal no longer gives a blessing worth sending.
        (Some((_, iat)), Some((_, sealed_at))) if sealed_at >= iat => {
            !still_gives_a_blessing(sealed_at, now, ttl)
        }
        (None, Some((_, sealed_at))) => !still_gives_a_blessing(sealed_at, now, ttl),
        (Some((_, iat)), _) => now >= iat.saturating_add(TOKEN_REFRESH.as_secs()),
        (None, None) => true,
    }
}

/// Whether a seal is wanted for the token held: it is for the current tree,
/// no seal at least as fresh has been accepted, and some key wants blessing.
fn seal_wanted(
    any_due: bool,
    nonce: &str,
    token: Option<(&str, u64)>,
    sealed: Option<(&str, u64)>,
) -> bool {
    let Some((token_nonce, iat)) = token else {
        return false;
    };
    any_due
        && token_nonce == nonce
        && !sealed
            .is_some_and(|(sealed_nonce, sealed_at)| sealed_nonce == nonce && sealed_at >= iat)
}

/// Why a seal posted to the status port was not taken.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Not the current binding's nonce.
    Nonce,
    /// The journal doesn't match the binding, or the seal is malformed, or
    /// a simulated `bless` reverts.
    Journal(String),
    /// No chain could be asked.
    Unavailable(String),
}

impl Refusal {
    fn status(&self) -> StatusCode {
        match self {
            Refusal::Nonce => StatusCode::CONFLICT,
            Refusal::Journal(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Refusal::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

/// What a prover posts.
#[derive(Debug, Deserialize)]
pub struct SealPost {
    pub nonce: String,
    pub seal: String,
    pub journal: String,
}

/// The registry's journal: `abi.encode(imageDigest, projectId, signer,
/// vaultsRoot, uint64 issuedAt)`, 160 bytes, each word's padding zero.
#[derive(Debug, PartialEq, Eq)]
struct Journal {
    image_digest: B256,
    project_id: B256,
    signer: Address,
    vaults_root: B256,
    issued_at: u64,
}

impl Journal {
    fn decode(bytes: &[u8]) -> Option<Journal> {
        if bytes.len() != 160
            || bytes[64..76].iter().any(|b| *b != 0)
            || bytes[128..152].iter().any(|b| *b != 0)
        {
            return None;
        }
        Some(Journal {
            image_digest: B256::from_slice(&bytes[0..32]),
            project_id: B256::from_slice(&bytes[32..64]),
            signer: Address::from_slice(&bytes[76..96]),
            vaults_root: B256::from_slice(&bytes[96..128]),
            issued_at: u64::from_be_bytes(bytes[152..160].try_into().ok()?),
        })
    }

    fn encode(&self) -> Vec<u8> {
        (
            self.image_digest,
            self.project_id,
            self.signer,
            self.vaults_root,
            self.issued_at,
        )
            .abi_encode_params()
    }
}

/// What a token's claims say, read without checking its signature: the
/// launcher handed it over a socket only this container reaches, and the
/// guest checks everything before anything is believed on a chain.
#[derive(Debug)]
struct Claims {
    issued_at: u64,
    nonces: Vec<String>,
    audience: String,
    image_digest: B256,
    project_id: B256,
}

fn claims(token: &str) -> Result<Claims> {
    let mut parts = token.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        bail!("not a compact JWT");
    };
    let json: serde_json::Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .context("the claims are not base64")?,
    )
    .context("the claims are not JSON")?;
    let nonces = match &json["eat_nonce"] {
        serde_json::Value::String(one) => vec![one.clone()],
        serde_json::Value::Array(many) => many
            .iter()
            .filter_map(|nonce| nonce.as_str().map(str::to_string))
            .collect(),
        _ => bail!("the token has no nonce"),
    };
    let digest = json
        .pointer("/submods/container/image_digest")
        .and_then(serde_json::Value::as_str)
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .context("the token names no image digest")?;
    let project = json
        .pointer("/submods/gce/project_id")
        .and_then(serde_json::Value::as_str)
        .context("the token names no project")?;
    Ok(Claims {
        issued_at: json["iat"].as_u64().context("the token has no iat")?,
        nonces,
        audience: json["aud"].as_str().unwrap_or_default().to_string(),
        image_digest: digest.parse().context("the image digest is not 32 bytes")?,
        project_id: keccak256(project.as_bytes()),
    })
}

/// Ask Confidential Space's launcher for a token whose nonce is `nonce`, over
/// its unix socket.
pub async fn request_token(socket: &Path, nonce: &str) -> Result<String> {
    use http_body_util::BodyExt;

    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to the launcher at {}", socket.display()))?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .context("speaking HTTP to the launcher")?;
    let _connection = crate::AbortOnDrop(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let body = serde_json::json!({
        "audience": AUDIENCE,
        "token_type": "OIDC",
        "nonces": [nonce],
    })
    .to_string();
    let request = http::Request::post("/v1/token")
        .header(http::header::HOST, "localhost")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(http_body_util::Full::new(bytes::Bytes::from(body)))?;
    let response = tokio::time::timeout(Duration::from_secs(30), sender.send_request(request))
        .await
        .context("the launcher did not answer")?
        .context("asking the launcher for a token")?;
    let status = response.status();
    let body = tokio::time::timeout(Duration::from_secs(30), response.into_body().collect())
        .await
        .context("the launcher did not finish its answer")?
        .context("reading the launcher's answer")?
        .to_bytes();
    anyhow::ensure!(body.len() < 64 * 1024, "the launcher's answer is too long");
    let text = String::from_utf8(body.to_vec()).context("the launcher's answer is not text")?;
    anyhow::ensure!(
        status.is_success(),
        "the launcher refused the token request ({status})"
    );
    Ok(text.trim().to_string())
}

/// The status port's view (see `GET /attestation`).
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Status {
    pub mode: String,
    pub binding: BindingView,
    pub nonce: String,
    pub token: Option<String>,
    pub token_issued_at: Option<u64>,
    pub seal: Option<String>,
    pub journal: Option<String>,
    pub seal_wanted: bool,
    pub leaves: Vec<LeafView>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct BindingView {
    pub evm_signer: String,
    pub solana_signer: String,
    pub vaults_root: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct LeafView {
    pub chain_id: u64,
    pub vault_chain: u64,
    pub vault: String,
    pub strategy_hash: String,
    pub key: String,
    pub leaf: String,
    pub proof: Vec<String>,
}

impl Attestation {
    pub fn new(mode: Mode) -> Result<Attestation> {
        Ok(Attestation {
            mode,
            instance: Arc::new(Key::fresh()?),
            inner: Default::default(),
            changed: Default::default(),
            chains: Default::default(),
        })
    }

    /// The key the journal names.
    pub fn instance(&self) -> Address {
        self.instance.address()
    }

    /// A chain served: where a `bless` is simulated, and (in development
    /// mode) where the journal is sealed.
    pub fn serve_chain(&self, client: Client, deployments: Deployments) {
        if let Ok(mut chains) = self.chains.lock() {
            chains.insert(client.chain_id, (client, deployments));
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn touched(&self, inner: &mut Inner) {
        inner.changed = Some(Instant::now());
        self.changed.notify_one();
    }

    /// Hold a key for a vault: its leaf joins the tree, wanting a blessing.
    /// The same leaf again changes nothing; a new key or bundle for the vault
    /// replaces its leaf.
    pub fn put(&self, chain_id: u64, vault: Address, strategy_hash: B256, key: Address) {
        let mut inner = self.inner();
        let leaf = Leaf {
            chain_id,
            vault_chain: chain_id,
            vault,
            strategy_hash,
            key,
            due: true,
        };
        let same = inner
            .leaves
            .get(&(chain_id, vault))
            .is_some_and(|held| held.key == key && held.strategy_hash == strategy_hash);
        if !same {
            inner.leaves.insert((chain_id, vault), leaf);
            self.touched(&mut inner);
        }
    }

    /// Let a vault's key go, if it is still `key`'s leaf.
    pub fn remove(&self, chain_id: u64, vault: Address, key: Address) {
        let mut inner = self.inner();
        if inner
            .leaves
            .get(&(chain_id, vault))
            .is_some_and(|held| held.key == key)
        {
            inner.leaves.remove(&(chain_id, vault));
            self.touched(&mut inner);
        }
    }

    /// Note whether a vault's key wants blessing, and the chain's TTL.
    pub fn note(&self, chain_id: u64, vault: Address, key: Address, wants: bool, ttl: u64) {
        let mut inner = self.inner();
        if ttl > 0 {
            inner.ttl = Some(inner.ttl.map_or(ttl, |held| held.min(ttl)));
        }
        if let Some(leaf) = inner.leaves.get_mut(&(chain_id, vault))
            && leaf.key == key
            && leaf.due != wants
        {
            leaf.due = wants;
            if wants {
                self.changed.notify_one();
            }
        }
    }

    fn leaves(inner: &Inner) -> Vec<Leaf> {
        inner.leaves.values().cloned().collect()
    }

    fn binding_of(&self, leaves: &[Leaf]) -> Binding {
        Binding {
            evm_signer: self.instance.address().into_array(),
            solana_signer: [0; 32],
            vaults_root: tree(leaves).0.0,
        }
    }

    /// What the instance's next token binds.
    pub fn binding(&self) -> Binding {
        self.binding_of(&Self::leaves(&self.inner()))
    }

    /// The image and the project this instance attests as, as far as it
    /// knows them: the development ones, or those its latest token names.
    pub fn identity(&self) -> Option<(B256, B256)> {
        match self.mode {
            Mode::Development => Some((
                chain::attest::image_digest("executor"),
                chain::attest::project_id(),
            )),
            Mode::Launcher => self
                .inner()
                .token
                .as_ref()
                .map(|held| (held.image_digest, held.project_id)),
        }
    }

    pub fn status(&self) -> Status {
        let inner = self.inner();
        let leaves = Self::leaves(&inner);
        let binding = self.binding_of(&leaves);
        let nonce = binding.nonce();
        let (_, proofs) = tree(&leaves);
        let any_due = leaves.iter().any(|leaf| leaf.due);
        let token = inner.token.as_ref().filter(|held| held.nonce == nonce);
        let sealed = inner.sealed.as_ref().filter(|sealed| sealed.nonce == nonce);
        let hex = |bytes: &[u8]| alloy::hex::encode_prefixed(bytes);
        Status {
            mode: match self.mode {
                Mode::Launcher => "launcher",
                Mode::Development => "development",
            }
            .into(),
            binding: BindingView {
                evm_signer: hex(&binding.evm_signer),
                solana_signer: hex(&binding.solana_signer),
                vaults_root: hex(&binding.vaults_root),
            },
            seal_wanted: self.mode == Mode::Launcher
                && seal_wanted(
                    any_due,
                    &nonce,
                    token.map(|held| (held.nonce.as_str(), held.issued_at)),
                    sealed.map(|sealed| (sealed.nonce.as_str(), sealed.issued_at)),
                ),
            nonce,
            token: token.map(|held| held.token.clone()),
            token_issued_at: token.map(|held| held.issued_at),
            seal: sealed.map(|sealed| hex(&sealed.seal)),
            journal: sealed.map(|sealed| hex(&sealed.journal)),
            leaves: leaves
                .iter()
                .zip(proofs)
                .map(|(leaf, proof)| LeafView {
                    chain_id: leaf.chain_id,
                    vault_chain: leaf.vault_chain,
                    vault: leaf.vault.to_string(),
                    strategy_hash: leaf.strategy_hash.to_string(),
                    key: leaf.key.to_string(),
                    leaf: leaf.hash().to_string(),
                    proof: proof.iter().map(ToString::to_string).collect(),
                })
                .collect(),
        }
    }

    /// Whether something is to be asked for now: a token in launcher mode, a
    /// development seal in development mode.
    fn wanted(&self, now: u64) -> bool {
        let inner = self.inner();
        if inner
            .changed
            .is_some_and(|changed| changed.elapsed() < SETTLE)
        {
            return false;
        }
        let leaves = Self::leaves(&inner);
        let nonce = self.binding_of(&leaves).nonce();
        let any_due = leaves.iter().any(|leaf| leaf.due);
        let ttl = inner.ttl.unwrap_or(DEFAULT_TTL);
        let sealed = inner
            .sealed
            .as_ref()
            .map(|sealed| (sealed.nonce.as_str(), sealed.issued_at));
        let token = match self.mode {
            Mode::Launcher => inner
                .token
                .as_ref()
                .map(|held| (held.nonce.as_str(), held.issued_at)),
            // In development mode the seal is made at once: no token waits.
            Mode::Development => None,
        };
        token_wanted(any_due, &nonce, token, sealed, now, ttl)
    }

    /// Keep a token, if it is for the current tree.
    fn hold(&self, token: String) -> Result<()> {
        let claims = claims(&token)?;
        anyhow::ensure!(
            claims.audience == AUDIENCE,
            "the token is for another audience"
        );
        let mut inner = self.inner();
        let nonce = self.binding_of(&Self::leaves(&inner)).nonce();
        if !claims.nonces.contains(&nonce) {
            bail!("the token binds a tree this instance no longer holds");
        }
        inner.token = Some(Held {
            token,
            nonce,
            issued_at: claims.issued_at,
            image_digest: claims.image_digest,
            project_id: claims.project_id,
        });
        Ok(())
    }

    /// Check a posted seal against the current binding, without a chain:
    /// what is left is the simulated `bless`. Says the journal, and the leaf
    /// and chain to simulate it on.
    fn check(&self, post: &SealPost) -> Result<(Sealed, Leaf, Vec<B256>), Refusal> {
        let inner = self.inner();
        let leaves = Self::leaves(&inner);
        let binding = self.binding_of(&leaves);
        let nonce = binding.nonce();
        if post.nonce != nonce {
            return Err(Refusal::Nonce);
        }
        let refused = |why: &str| Refusal::Journal(why.to_string());
        let seal: Bytes = post
            .seal
            .parse()
            .map_err(|_| refused("the seal is not hex"))?;
        let journal_bytes: Bytes = post
            .journal
            .parse()
            .map_err(|_| refused("the journal is not hex"))?;
        if seal.is_empty() {
            return Err(refused("the seal is empty"));
        }
        let journal =
            Journal::decode(&journal_bytes).ok_or_else(|| refused("the journal is malformed"))?;
        if journal.signer != self.instance.address() {
            return Err(refused("the journal names another instance"));
        }
        if journal.vaults_root != B256::from(binding.vaults_root) {
            return Err(refused("the journal is for another tree"));
        }
        if let Some((image, project)) = match self.mode {
            Mode::Development => Some((
                chain::attest::image_digest("executor"),
                chain::attest::project_id(),
            )),
            Mode::Launcher => {
                let held = inner
                    .token
                    .as_ref()
                    .filter(|held| held.nonce == nonce)
                    .ok_or_else(|| refused("no token is held for this tree"))?;
                if journal.issued_at != held.issued_at {
                    return Err(refused("the journal is for another token"));
                }
                Some((held.image_digest, held.project_id))
            }
        } && (journal.image_digest != image || journal.project_id != project)
        {
            return Err(refused("the journal names another image or project"));
        }
        let (_, proofs) = tree(&leaves);
        let (Some(leaf), Some(proof)) = (leaves.first(), proofs.first()) else {
            return Err(refused("the instance holds no key"));
        };
        Ok((
            Sealed {
                nonce,
                seal,
                journal: journal_bytes,
                issued_at: journal.issued_at,
            },
            leaf.clone(),
            proof.clone(),
        ))
    }

    /// Take a seal a prover posts: checked against the binding, then a
    /// `bless` of the first leaf simulated on its chain; kept only if it
    /// would land.
    pub async fn accept(&self, post: SealPost) -> Result<(), Refusal> {
        let (sealed, leaf, proof) = self.check(&post)?;
        let chain = self
            .chains
            .lock()
            .ok()
            .and_then(|chains| chains.get(&leaf.chain_id).cloned());
        let Some((client, deployments)) = chain else {
            return Err(Refusal::Unavailable(format!(
                "chain {} is not connected",
                leaf.chain_id
            )));
        };
        let simulated = client
            .read(
                deployments.attestation,
                IAttestationRegistry::blessCall {
                    seal: sealed.seal.clone(),
                    journal: sealed.journal.clone(),
                    vaultChain: U256::from(leaf.vault_chain),
                    vault: leaf.vault,
                    strategyHash: leaf.strategy_hash,
                    key: leaf.key,
                    merkleProof: proof,
                },
            )
            .await;
        if let Err(error) = simulated {
            let why = format!("{error:#}");
            return Err(if why.contains("revert") {
                Refusal::Journal(format!("a simulated bless reverts: {why}"))
            } else {
                Refusal::Unavailable(why)
            });
        }
        self.keep(sealed)
    }

    fn keep(&self, sealed: Sealed) -> Result<(), Refusal> {
        let mut inner = self.inner();
        // The tree may have moved while the chain was asked.
        if self.binding_of(&Self::leaves(&inner)).nonce() != sealed.nonce {
            return Err(Refusal::Nonce);
        }
        tracing::info!(
            issued_at = sealed.issued_at,
            "a seal for the instance's keys is accepted"
        );
        inner.sealed = Some(sealed);
        Ok(())
    }

    /// Seal the journal with the attestor role, as the development checker
    /// trusts, dated by the served chains' clocks.
    async fn seal_development(&self) -> Result<()> {
        let chains: Vec<(Client, Deployments)> = self
            .chains
            .lock()
            .map(|chains| chains.values().cloned().collect())
            .unwrap_or_default();
        let Some((first, deployments)) = chains.first().cloned() else {
            bail!("no chain is connected");
        };
        let mut issued_at = u64::MAX;
        for (client, _) in &chains {
            issued_at = issued_at.min(client.block_timestamp().await?);
        }
        let issued_at = issued_at.saturating_sub(DATED_BEFORE_THE_HEAD);
        let (nonce, root) = {
            let inner = self.inner();
            let binding = self.binding_of(&Self::leaves(&inner));
            (binding.nonce(), B256::from(binding.vaults_root))
        };
        let journal = Journal {
            image_digest: chain::attest::image_digest("executor"),
            project_id: chain::attest::project_id(),
            signer: self.instance.address(),
            vaults_root: root,
            issued_at,
        }
        .encode();
        let seal = first
            .sign(deployments.roles.attestor, keccak256(&journal))
            .await
            .context("sealing the journal with the attestor role")?;
        self.keep(Sealed {
            nonce,
            seal,
            journal: journal.into(),
            issued_at,
        })
        .map_err(|_| anyhow!("the tree moved while the journal was sealed"))
    }

    /// Ask for a token (or, in development mode, seal) whenever the tree
    /// changes or a blessing is due, for as long as the executor runs.
    pub async fn run(self, socket: PathBuf) {
        let mut failures: u32 = 0;
        let mut next_try = Instant::now();
        let mut said = crate::Throttle::default();
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                _ = self.changed.notified() => {
                    tokio::time::sleep(SETTLE).await;
                }
            }
            if Instant::now() < next_try || !self.wanted(unix_now()) {
                continue;
            }
            let done = match self.mode {
                Mode::Launcher => {
                    let nonce = self.binding().nonce();
                    match request_token(&socket, &nonce).await {
                        Ok(token) => self.hold(token).map(|()| "a token from the launcher"),
                        Err(error) => Err(error),
                    }
                }
                Mode::Development => self.seal_development().await.map(|()| "a development seal"),
            };
            match done {
                Ok(what) => {
                    failures = 0;
                    next_try = Instant::now() + Duration::from_secs(10);
                    tracing::info!(root = %B256::from(self.binding().vaults_root), "the instance's keys are bound in {what}");
                }
                Err(error) => {
                    failures += 1;
                    next_try = Instant::now() + crate::backoff(failures);
                    if said.due(Instant::now()) {
                        tracing::warn!(
                            error = format!("{error:#}"),
                            "could not attest the instance's keys; trying again"
                        );
                    }
                }
            }
        }
    }
}

/// `GET /attestation` and `POST /attestation/seal`.
pub fn routes(attestation: Attestation) -> axum::Router {
    let reading = attestation.clone();
    axum::Router::new()
        .route(
            "/attestation",
            axum::routing::get(move || {
                let attestation = reading.clone();
                async move { axum::Json(attestation.status()) }
            }),
        )
        .route(
            "/attestation/seal",
            axum::routing::post(move |axum::Json(post): axum::Json<SealPost>| {
                let attestation = attestation.clone();
                async move {
                    match attestation.accept(post).await {
                        Ok(()) => (StatusCode::NO_CONTENT, String::new()),
                        Err(refusal) => {
                            let why = match &refusal {
                                Refusal::Nonce => "not the current binding's nonce".to_string(),
                                Refusal::Journal(why) | Refusal::Unavailable(why) => why.clone(),
                            };
                            tracing::info!(why, "a posted seal was not taken");
                            (refusal.status(), why)
                        }
                    }
                }
            }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: u64 = 72 * 3600;
    const NOW: u64 = 1_800_000_000;

    fn leaf(chain_id: u64, vault: u8, key: u8) -> (u64, Address, B256, Address) {
        (
            chain_id,
            Address::repeat_byte(vault),
            B256::repeat_byte(0x5b),
            Address::repeat_byte(key),
        )
    }

    fn with(leaves: &[(u64, Address, B256, Address)]) -> Attestation {
        let attestation = Attestation::new(Mode::Launcher).unwrap();
        for (chain_id, vault, strategy, key) in leaves {
            attestation.put(*chain_id, *vault, *strategy, *key);
        }
        attestation
    }

    /// OpenZeppelin's `MerkleProof.verify`.
    fn verifies(proof: &[B256], root: B256, leaf: B256) -> bool {
        let mut at = leaf;
        for sibling in proof {
            let (low, high) = if at <= *sibling {
                (at, *sibling)
            } else {
                (*sibling, at)
            };
            at = keccak256([low.as_slice(), high.as_slice()].concat());
        }
        at == root
    }

    #[test]
    fn every_leaf_names_its_key_by_the_registry_s_rule_and_proves_under_the_bound_root() {
        let attestation = with(&[
            leaf(31337, 0x11, 0xa1),
            leaf(31337, 0x22, 0xa2),
            leaf(84532, 0x11, 0xa3),
            leaf(46630, 0x33, 0xa4),
            leaf(46630, 0x44, 0xa5),
        ]);
        let status = attestation.status();
        assert_eq!(status.leaves.len(), 5);
        let root: B256 = status.binding.vaults_root.parse().unwrap();
        assert_eq!(B256::from(attestation.binding().vaults_root), root);
        for view in &status.leaves {
            let key: Address = view.key.parse().unwrap();
            let vault: Address = view.vault.parse().unwrap();
            let strategy: B256 = view.strategy_hash.parse().unwrap();
            let expected = keccak256(keccak256(
                (
                    U256::from(view.vault_chain),
                    vault,
                    strategy,
                    U256::from(view.chain_id),
                    key,
                )
                    .abi_encode(),
            ));
            assert_eq!(view.leaf.parse::<B256>().unwrap(), expected);
            let proof: Vec<B256> = view
                .proof
                .iter()
                .map(|node| node.parse().unwrap())
                .collect();
            assert!(verifies(&proof, root, expected), "a proof does not verify");
            // A proof proves its own leaf only.
            let other = keccak256(keccak256(
                (
                    U256::from(view.vault_chain),
                    vault,
                    strategy,
                    U256::from(view.chain_id),
                    Address::ZERO,
                )
                    .abi_encode(),
            ));
            assert!(!verifies(&proof, root, other));
        }
        assert_eq!(status.nonce, attestation.binding().nonce());
        assert_eq!(
            status.binding.evm_signer,
            attestation.instance().to_string().to_lowercase()
        );
    }

    #[test]
    fn the_tree_changes_with_a_key_or_a_bundle_and_only_then() {
        let attestation = with(&[leaf(31337, 0x11, 0xa1)]);
        let first = attestation.binding().nonce();
        attestation.put(
            31337,
            Address::repeat_byte(0x11),
            B256::repeat_byte(0x5b),
            Address::repeat_byte(0xa1),
        );
        assert_eq!(attestation.binding().nonce(), first);
        attestation.put(
            31337,
            Address::repeat_byte(0x11),
            B256::repeat_byte(0x5c),
            Address::repeat_byte(0xa1),
        );
        let second = attestation.binding().nonce();
        assert_ne!(second, first);
        attestation.put(
            31337,
            Address::repeat_byte(0x11),
            B256::repeat_byte(0x5c),
            Address::repeat_byte(0xa2),
        );
        assert_ne!(attestation.binding().nonce(), second);
        // A key that is no longer the vault's lets nothing go.
        attestation.remove(
            31337,
            Address::repeat_byte(0x11),
            Address::repeat_byte(0xa1),
        );
        assert_eq!(attestation.status().leaves.len(), 1);
        attestation.remove(
            31337,
            Address::repeat_byte(0x11),
            Address::repeat_byte(0xa2),
        );
        assert!(attestation.status().leaves.is_empty());
        assert_eq!(attestation.binding().vaults_root, [0; 32]);
    }

    #[test]
    fn a_token_is_asked_for_when_a_key_wants_blessing_and_nothing_held_answers_it() {
        let n = "n";
        // Nothing wants blessing: nothing is asked for.
        assert!(!token_wanted(false, n, None, None, NOW, TTL));
        // A key wants blessing and nothing is held.
        assert!(token_wanted(true, n, None, None, NOW, TTL));
        // A token for another tree answers nothing.
        assert!(token_wanted(true, n, Some(("old", NOW)), None, NOW, TTL));
        // A token waiting for its seal is enough, until it is 40 minutes old.
        assert!(!token_wanted(
            true,
            n,
            Some((n, NOW)),
            None,
            NOW + 39 * 60,
            TTL
        ));
        assert!(token_wanted(
            true,
            n,
            Some((n, NOW)),
            None,
            NOW + 40 * 60,
            TTL
        ));
        // A seal for the tree answers it while it still gives a blessing
        // with more than a third left.
        let sealed = Some((n, NOW));
        assert!(!token_wanted(
            true,
            n,
            Some((n, NOW)),
            sealed,
            NOW + TTL * 2 / 3 - 1,
            TTL
        ));
        assert!(token_wanted(
            true,
            n,
            Some((n, NOW)),
            sealed,
            NOW + TTL * 2 / 3,
            TTL
        ));
        assert!(!token_wanted(true, n, None, sealed, NOW + 60, TTL));
        // A seal for another tree answers nothing.
        assert!(token_wanted(
            true,
            n,
            None,
            Some(("old", NOW)),
            NOW + 60,
            TTL
        ));
    }

    #[test]
    fn a_seal_is_wanted_for_a_token_of_the_current_tree_with_none_as_fresh_accepted() {
        let n = "n";
        assert!(!seal_wanted(true, n, None, None));
        assert!(seal_wanted(true, n, Some((n, NOW)), None));
        assert!(!seal_wanted(false, n, Some((n, NOW)), None));
        assert!(!seal_wanted(true, n, Some(("old", NOW)), None));
        assert!(!seal_wanted(true, n, Some((n, NOW)), Some((n, NOW))));
        // A seal of an older token of the same tree leaves the newer wanted.
        assert!(seal_wanted(true, n, Some((n, NOW + 10)), Some((n, NOW))));
    }

    #[test]
    fn a_key_wants_blessing_unblessed_or_with_a_third_or_less_left() {
        assert!(due(false, NOW + TTL, NOW, TTL));
        assert!(!due(true, NOW + TTL / 3 + 1, NOW, TTL));
        assert!(due(true, NOW + TTL / 3, NOW, TTL));
        assert!(due(true, NOW - 1, NOW, TTL));
    }

    /// A token as the launcher would hand it out (unsigned: nothing here
    /// checks the signature).
    fn token(nonce: &str, iat: u64) -> String {
        let b64 = |value: serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
        };
        format!(
            "{}.{}.c2ln",
            b64(serde_json::json!({"alg": "RS256", "kid": "k"})),
            b64(serde_json::json!({
                "aud": AUDIENCE,
                "iat": iat,
                "exp": iat + 3600,
                "eat_nonce": nonce,
                "submods": {
                    "container": {"image_digest": format!("sha256:{}", "ab".repeat(32))},
                    "gce": {"project_id": "shonin-testnet"}
                }
            }))
        )
    }

    fn journal_for(attestation: &Attestation, issued_at: u64) -> Journal {
        Journal {
            image_digest: B256::repeat_byte(0xab),
            project_id: keccak256("shonin-testnet"),
            signer: attestation.instance(),
            vaults_root: attestation.binding().vaults_root.into(),
            issued_at,
        }
    }

    fn post(nonce: String, journal: &Journal) -> SealPost {
        SealPost {
            nonce,
            seal: alloy::hex::encode_prefixed([7u8; 324]),
            journal: alloy::hex::encode_prefixed(journal.encode()),
        }
    }

    #[test]
    fn a_token_is_held_only_for_the_current_tree_and_then_wants_a_seal() {
        let attestation = with(&[leaf(31337, 0x11, 0xa1)]);
        let nonce = attestation.binding().nonce();
        assert!(attestation.hold(token("another", NOW)).is_err());
        assert!(attestation.status().token.is_none());
        attestation.hold(token(&nonce, NOW)).unwrap();
        let status = attestation.status();
        assert_eq!(status.token_issued_at, Some(NOW));
        assert!(status.seal_wanted);
        assert_eq!(
            attestation.identity(),
            Some((B256::repeat_byte(0xab), keccak256("shonin-testnet")))
        );
        // The tree moves on: the token no longer answers for it.
        attestation.put(
            31337,
            Address::repeat_byte(0x22),
            B256::ZERO,
            Address::repeat_byte(0xa2),
        );
        let status = attestation.status();
        assert!(status.token.is_none());
        assert!(!status.seal_wanted);
    }

    #[test]
    fn a_seal_for_another_nonce_or_another_binding_is_refused() {
        let attestation = with(&[leaf(31337, 0x11, 0xa1)]);
        let nonce = attestation.binding().nonce();
        attestation.hold(token(&nonce, NOW)).unwrap();
        let good = journal_for(&attestation, NOW);
        assert!(attestation.check(&post(nonce.clone(), &good)).is_ok());

        assert_eq!(
            attestation.check(&post("ab".repeat(32), &good)).err(),
            Some(Refusal::Nonce)
        );
        let refused = |journal: Journal| {
            matches!(
                attestation.check(&post(nonce.clone(), &journal)),
                Err(Refusal::Journal(_))
            )
        };
        assert!(refused(Journal {
            vaults_root: B256::repeat_byte(1),
            ..journal_for(&attestation, NOW)
        }));
        assert!(refused(Journal {
            signer: Address::repeat_byte(1),
            ..journal_for(&attestation, NOW)
        }));
        assert!(
            refused(journal_for(&attestation, NOW + 1)),
            "a journal for another token"
        );
        assert!(refused(Journal {
            image_digest: B256::repeat_byte(1),
            ..journal_for(&attestation, NOW)
        }));
        assert!(refused(Journal {
            project_id: B256::repeat_byte(1),
            ..journal_for(&attestation, NOW)
        }));
        let mut malformed = post(nonce.clone(), &good);
        malformed.journal = alloy::hex::encode_prefixed([0u8; 159]);
        assert!(matches!(
            attestation.check(&malformed),
            Err(Refusal::Journal(_))
        ));
    }

    #[tokio::test]
    async fn the_seal_route_answers_conflict_for_another_nonce_and_unprocessable_for_another_binding()
     {
        use tower::ServiceExt;
        let attestation = with(&[leaf(31337, 0x11, 0xa1)]);
        let nonce = attestation.binding().nonce();
        attestation.hold(token(&nonce, NOW)).unwrap();
        let app = routes(attestation.clone());
        let ask = |body: SealPost| {
            let app = app.clone();
            async move {
                let body = serde_json::json!({"nonce": body.nonce, "seal": body.seal, "journal": body.journal});
                app.oneshot(
                    http::Request::post("/attestation/seal")
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
            }
        };
        let good = journal_for(&attestation, NOW);
        assert_eq!(
            ask(post("00".repeat(32), &good)).await,
            StatusCode::CONFLICT
        );
        let other = Journal {
            vaults_root: B256::repeat_byte(1),
            ..journal_for(&attestation, NOW)
        };
        assert_eq!(
            ask(post(nonce.clone(), &other)).await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        // A journal that matches still has to pass a simulated bless, and no
        // chain is connected here.
        assert_eq!(
            ask(post(nonce, &good)).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(attestation.status().seal.is_none());
    }

    #[test]
    fn no_key_s_secret_is_ever_in_what_the_status_port_serves() {
        let vault_key = crate::keys::Key::fresh().unwrap();
        let attestation = with(&[]);
        attestation.put(
            31337,
            Address::repeat_byte(0x11),
            B256::ZERO,
            vault_key.address(),
        );
        let served = serde_json::to_string(&attestation.status())
            .unwrap()
            .to_lowercase();
        for secret in [
            alloy::hex::encode(vault_key.signer().to_bytes()),
            alloy::hex::encode(attestation.instance.signer().to_bytes()),
        ] {
            assert!(!served.contains(&secret));
        }
        assert!(served.contains(&vault_key.address().to_string().to_lowercase()));
    }

    #[tokio::test]
    async fn the_launcher_is_asked_over_its_socket_for_a_token_of_our_audience_and_nonce() {
        let dir = std::env::temp_dir().join(format!("shonin-launcher-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("teeserver.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let asked: Arc<Mutex<Option<serde_json::Value>>> = Default::default();
        let noted = asked.clone();
        let app = axum::Router::new().route(
            "/v1/token",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let noted = noted.clone();
                async move {
                    let nonce = body["nonces"][0].as_str().unwrap_or_default().to_string();
                    *noted.lock().unwrap() = Some(body);
                    token(&nonce, NOW)
                }
            }),
        );
        let _server = crate::AbortOnDrop(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));

        let attestation = with(&[leaf(31337, 0x11, 0xa1)]);
        let nonce = attestation.binding().nonce();
        let token = request_token(&socket, &nonce).await.unwrap();
        attestation.hold(token).unwrap();
        let asked = asked.lock().unwrap().clone().unwrap();
        assert_eq!(asked["audience"], AUDIENCE);
        assert_eq!(asked["token_type"], "OIDC");
        assert_eq!(asked["nonces"], serde_json::json!([nonce]));
        assert_eq!(attestation.status().token_issued_at, Some(NOW));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
