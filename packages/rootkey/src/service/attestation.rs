//! A service's attestation: one leaf per chain it serves, the token that
//! binds them, and the seal that proves the token.
//!
//! A builder or a reviewer holds one key (its [`super::Identity`]'s signer)
//! for every chain it serves. Each chain gets its own leaf naming that key
//! and no vault (`chain::attest::key_leaf(0, 0, 0, chain id, key)`), the
//! instance binds a fresh key of its own and the root over those leaves into
//! the nonce of the token it asks Confidential Space for, and one proof of
//! that token blesses the key on every chain.
//!
//! On a Solana cluster the service holds its Solana key (its
//! [`super::Identity`]'s ed25519 key), and the cluster gets a leaf naming
//! that key and no vault, in the Solana registry's layout
//! (`chain::solana::Platform::service_leaf`), after the EVM leaves. The
//! instance's own Solana key is the token's Solana signer, and the same proof
//! blesses the key there: the Solana registry reads it with the journal laid
//! out its own way (`chain::solana::attest::SolanaSeal::from_evm`), published
//! on the status port beside the leaves, in the executor's shape.
//!
//! As the executor does, the service never blesses anything itself: it
//! publishes the leaves, the token and, once a prover has posted one, the
//! seal on its status port (`GET /attestation`, in the executor's shape), and
//! the keeper sends each `bless` and stocks the blessed key with gas. In
//! development mode, on a chain with the development checker, the service
//! seals its journal with the attestor role instead of asking for a token,
//! and the keeper blesses from that seal the same way.
//!
//! A leaf is due while the registry doesn't count the key as blessed as the
//! service's kind (`isBlessedAs`), or its blessing has half its length or
//! less left: a service that is started now and then renews early enough
//! that the waker's renewal boot ([`ServiceAttestation::renew_at`]) finds it
//! still blessed.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol_types::SolValue;
use anyhow::{Context, Result, anyhow, bail};
use axum::http::StatusCode;
use base64::Engine;
use chain::bindings::attestation::IAttestationRegistry;
use chain::solana::attest::{KeyLeaf, SolanaSeal};
use chain::solana::{Keypair as SolanaKeypair, Platform, Pubkey, Signer as _, SolanaClient};
use chain::{Client, Deployments};
use serde::{Deserialize, Serialize};
use shonin_attestation_core::Binding;

use crate::chain_view::ImageKind;
use crate::launcher::{AUDIENCE, request_token};

/// A token is good for an hour, and a proof must be made within it: one
/// older than this, while a seal is still wanted, is asked for again so a
/// prover that starts late still has time to prove it.
const TOKEN_REFRESH: Duration = Duration::from_secs(40 * 60);

/// How often the chains are read for the key's blessing, besides whenever
/// something changes.
const READ_EVERY: Duration = if cfg!(test) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(15)
};

/// How often the loop looks whether a token is wanted.
const TICK: Duration = if cfg!(test) {
    Duration::from_millis(50)
} else {
    Duration::from_secs(2)
};

/// How far before the chains' head a development seal is dated, as
/// `chain::attest` does: a public node a block behind would see it as from
/// the future.
const DATED_BEFORE_THE_HEAD: u64 = 60;

/// How the service attests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// In Confidential Space: a token from the launcher, proved by a prover.
    Launcher,
    /// On a development chain: the journal sealed with the attestor role,
    /// which the development checker trusts.
    Development,
}

/// The key's standing on one chain, as last read there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Leaf {
    /// Whether the chain has been read since the leaf was added.
    read: bool,
    /// `isBlessedAs(key, kind)`.
    blessed: bool,
    /// When the key's blessing runs out (`keyInfo(key).blessedUntil`); zero
    /// while it has none.
    until: u64,
    /// The chain's time as last read; `None` if it couldn't be.
    chain_now: Option<u64>,
    /// The chain's blessing TTL.
    ttl: u64,
}

impl Leaf {
    fn due(&self) -> bool {
        !self.read
            || self
                .chain_now
                .is_some_and(|now| due(self.blessed, self.until, now, self.ttl))
            || (self.chain_now.is_none() && !self.blessed)
    }
}

/// A token the launcher gave, for the nonce it binds.
#[derive(Clone, Debug)]
struct Held {
    token: String,
    nonce: String,
    issued_at: u64,
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

/// The service's leaf on one Solana cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClusterLeaf {
    /// What stands for the cluster in the leaf (`keccak256("solana:" +
    /// name)`).
    value: [u8; 32],
    leaf: Leaf,
}

#[derive(Default)]
struct Inner {
    /// By chain id, so the tree's order is the same at every look.
    leaves: BTreeMap<u64, Leaf>,
    /// By the cluster's chain key (`solana:devnet`), after the EVM leaves.
    clusters: BTreeMap<String, ClusterLeaf>,
    token: Option<Held>,
    sealed: Option<Sealed>,
}

/// A Solana cluster served: where the key's blessing is read and a `bless`
/// simulated, with whoever relays it paying.
#[derive(Clone)]
pub struct ClusterServed {
    pub client: SolanaClient,
    pub platform: Platform,
    /// Who pays for a `bless` there: the keeper, who sends it.
    pub payer: Pubkey,
}

/// A builder's or a reviewer's attestation, shared by its run loop and its
/// status port.
#[derive(Clone)]
pub struct ServiceAttestation {
    kind: ImageKind,
    key: Address,
    /// The key blessed on a Solana cluster: the service's ed25519 key.
    solana_key: Pubkey,
    mode: Mode,
    /// The key the journal names. It identifies the instance and signs
    /// nothing.
    instance: Address,
    /// The instance's Solana key, the token's Solana signer: it identifies
    /// the instance to a Solana registry, and signs nothing either.
    solana_instance: Pubkey,
    inner: Arc<Mutex<Inner>>,
    changed: Arc<tokio::sync::Notify>,
    chains: Arc<Mutex<BTreeMap<u64, (Client, Deployments)>>>,
    clusters: Arc<Mutex<BTreeMap<String, ClusterServed>>>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// The leaf for `key` on chain `chain_id`: no vault, no bundle, as the
/// registry's `bless` takes a builder's or a reviewer's.
pub fn service_leaf(chain_id: u64, key: Address) -> B256 {
    chain::attest::key_leaf(
        U256::ZERO,
        Address::ZERO,
        B256::ZERO,
        U256::from(chain_id),
        key,
    )
}

/// The leaf for the Solana key `key` on the cluster `cluster` stands for:
/// no vault, no bundle, as the Solana registry's `bless` takes a builder's or
/// a reviewer's (`chain::solana::Platform::service_leaf`).
pub fn solana_service_leaf(cluster: &[u8; 32], key: &Pubkey) -> B256 {
    B256::from(chain::solana::programs::common::digest::blessing_leaf(
        &[0; 32],
        &[0; 32],
        &[0; 32],
        cluster,
        &key.to_bytes(),
    ))
}

/// Whether a blessing that ends at `until` wants renewing at `now`: not
/// blessed as the service's kind at all, or half of `ttl` or less left.
pub fn due(blessed: bool, until: u64, now: u64, ttl: u64) -> bool {
    !blessed || until < now.saturating_add(ttl / 2)
}

/// A chain whose key wants blessing, as it last said.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Wanting {
    chain_now: Option<u64>,
    until: u64,
    ttl: u64,
}

/// Whether a seal dated `sealed_at` answers a chain that wants blessing: the
/// chain hasn't applied it yet, and a blessing from it would not be due again
/// at once by that chain's clock. A seal the chain has applied, while the key
/// still wants blessing, answers nothing: only a fresh attestation renews it.
/// A chain whose clock couldn't be read is taken to be answered until it can.
fn seal_answers(sealed_at: u64, wanting: Wanting) -> bool {
    let Some(now) = wanting.chain_now else {
        return true;
    };
    let applied = wanting.until >= sealed_at.saturating_add(wanting.ttl);
    !applied && sealed_at.saturating_add(wanting.ttl / 2) > now
}

/// Whether to ask for a token (or, in development mode, a seal) now: some
/// chain wants blessing, and neither the token held for the current tree,
/// waiting for its seal, nor the seal accepted for it answers that.
fn token_wanted(
    wanting: &[Wanting],
    nonce: &str,
    token: Option<(&str, u64)>,
    sealed: Option<(&str, u64)>,
    wall_now: u64,
) -> bool {
    if wanting.is_empty() {
        return false;
    }
    let sealed = sealed.filter(|(for_nonce, _)| *for_nonce == nonce);
    let token = token.filter(|(for_nonce, _)| *for_nonce == nonce);
    let answered = |sealed_at: u64| {
        wanting
            .iter()
            .all(|wanting| seal_answers(sealed_at, *wanting))
    };
    match (token, sealed) {
        (Some((_, iat)), Some((_, sealed_at))) if sealed_at >= iat => !answered(sealed_at),
        (None, Some((_, sealed_at))) => !answered(sealed_at),
        (Some((_, iat)), _) => wall_now >= iat.saturating_add(TOKEN_REFRESH.as_secs()),
        (None, None) => true,
    }
}

/// When a development seal is dated, with the chains' head at `head`: a
/// minute before it.
fn dated(head: u64) -> u64 {
    head.saturating_sub(DATED_BEFORE_THE_HEAD)
}

/// A chain to simulate a `bless` on, and its leaf's proof.
type Simulate = (u64, Vec<B256>);

/// A Solana cluster to simulate a `bless` on, and its leaf's proof.
type SimulateCluster = (String, Vec<B256>);

/// Why a seal posted to the status port was not taken.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Not the current binding's nonce.
    Nonce,
    /// The journal doesn't match the binding, or the seal is malformed, or
    /// a simulated `bless` reverts.
    Journal(String),
    /// A chain could not be asked.
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
#[derive(Debug, Deserialize, Serialize)]
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

/// The status port's view (`GET /attestation`): the executor's, field for
/// field, with no root and one leaf per chain naming no vault.
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
    /// Always `null`: a service holds no root key.
    pub root: Option<serde_json::Value>,
    /// Each Solana cluster's leaf, with the journal and seal its registry
    /// takes, as the executor publishes its own.
    pub solana: Vec<SolanaView>,
}

/// One Solana cluster's part of the status: the executor's shape.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct SolanaView {
    /// The cluster's chain key.
    pub cluster: String,
    /// The Solana registry's journal (Borsh, hex), once a seal is held.
    pub journal: Option<String>,
    pub seal: Option<SolanaSeal>,
    pub leaves: Vec<SolanaLeafView>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct SolanaLeafView {
    /// The zero key: a service serves no vault.
    pub vault: String,
    pub strategy_hash: String,
    pub key: String,
    pub due: bool,
    pub leaf: String,
    pub proof: Vec<String>,
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

/// The name a development image stands for, as `chain::attest` digests it.
fn development_name(kind: ImageKind) -> &'static str {
    match kind {
        ImageKind::Builder => "builder",
        ImageKind::Reviewer => "reviewer",
        ImageKind::Executor => "executor",
        ImageKind::Unwinder => "unwinder",
        ImageKind::Holder => "holder",
    }
}

impl ServiceAttestation {
    /// The attestation of a service of `kind` (a builder or a reviewer)
    /// whose keys are `key` and, on Solana, `solana_key`, in launcher mode,
    /// with fresh instance keys.
    pub fn new(kind: ImageKind, key: Address, solana_key: Pubkey) -> Result<ServiceAttestation> {
        if !matches!(kind, ImageKind::Builder | ImageKind::Reviewer) {
            bail!("only a builder or a reviewer attests as a service, not {kind}");
        }
        let mut secret = zeroize::Zeroizing::new([0u8; 32]);
        getrandom::fill(secret.as_mut()).map_err(|_| anyhow!("no randomness for the instance"))?;
        let solana_instance = SolanaKeypair::new_from_array(*secret).pubkey();
        Ok(ServiceAttestation {
            kind,
            key,
            solana_key,
            mode: Mode::Launcher,
            instance: PrivateKeySigner::random().address(),
            solana_instance,
            inner: Default::default(),
            changed: Default::default(),
            chains: Default::default(),
            clusters: Default::default(),
        })
    }

    /// The same, sealing in development mode: with the attestor role, as the
    /// development checker trusts, instead of asking for a token.
    pub fn development(mut self) -> ServiceAttestation {
        self.mode = Mode::Development;
        self
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn kind(&self) -> ImageKind {
        self.kind
    }

    /// The key blessed: the service's signer.
    pub fn key(&self) -> Address {
        self.key
    }

    /// The key the journal names.
    pub fn instance(&self) -> Address {
        self.instance
    }

    /// The key blessed on a Solana cluster.
    pub fn solana_key(&self) -> Pubkey {
        self.solana_key
    }

    /// The instance's Solana key, the token's Solana signer.
    pub fn solana_instance(&self) -> Pubkey {
        self.solana_instance
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A chain served: its leaf joins the tree, and the key's blessing is
    /// read there, a `bless` simulated there before a seal is kept, and (in
    /// development mode) the journal sealed there.
    pub fn serve_chain(&self, client: Client, deployments: Deployments) {
        let chain_id = client.chain_id;
        if let Ok(mut chains) = self.chains.lock() {
            chains.insert(chain_id, (client, deployments));
        }
        self.serve_chain_id(chain_id);
    }

    /// A chain served, known by its id alone: its leaf joins the tree, but
    /// nothing is read or simulated there. For tests without a chain, with
    /// [`ServiceAttestation::note`] standing for the reads.
    #[doc(hidden)]
    pub fn serve_chain_id(&self, chain_id: u64) {
        let mut inner = self.inner();
        if let std::collections::btree_map::Entry::Vacant(entry) = inner.leaves.entry(chain_id) {
            entry.insert(Leaf {
                read: false,
                blessed: false,
                until: 0,
                chain_now: None,
                ttl: 0,
            });
            drop(inner);
            self.changed.notify_one();
        }
    }

    /// Note what chain `chain_id` says of the key: whether it is blessed as
    /// the service's kind, when its blessing runs out, the chain's time
    /// (`None` if it couldn't be read) and its blessing TTL. The run loop
    /// notes each served chain itself.
    pub fn note(&self, chain_id: u64, blessed: bool, until: u64, chain_now: Option<u64>, ttl: u64) {
        let mut inner = self.inner();
        if let Some(leaf) = inner.leaves.get_mut(&chain_id) {
            let was_due = leaf.due();
            leaf.read = true;
            leaf.chain_now = chain_now;
            if chain_now.is_some() {
                leaf.blessed = blessed;
                leaf.until = until;
                leaf.ttl = ttl;
            }
            if leaf.due() && !was_due {
                drop(inner);
                self.changed.notify_one();
            }
        }
    }

    /// Whether the key is blessed as the service's kind on `chain_id`, as
    /// last read there: until then, no work is taken on that chain.
    pub fn ready(&self, chain_id: u64) -> bool {
        self.inner()
            .leaves
            .get(&chain_id)
            .is_some_and(|leaf| leaf.read && leaf.blessed)
    }

    /// A Solana cluster served, by its chain key (`solana:devnet`): its leaf
    /// joins the tree, and the Solana key's blessing is read there and a
    /// `bless` simulated there, `payer` paying, before a seal is kept.
    pub fn serve_cluster(&self, cluster: String, served: ClusterServed) {
        let value = served.platform.cluster;
        if let Ok(mut clusters) = self.clusters.lock() {
            clusters.insert(cluster.clone(), served);
        }
        self.serve_cluster_value(cluster, value);
    }

    /// A Solana cluster served, known by its chain key and the value that
    /// stands for it alone: its leaf joins the tree, but nothing is read or
    /// simulated there. For tests without a cluster, with
    /// [`ServiceAttestation::note_cluster`] standing for the reads.
    #[doc(hidden)]
    pub fn serve_cluster_value(&self, cluster: String, value: [u8; 32]) {
        let mut inner = self.inner();
        if let std::collections::btree_map::Entry::Vacant(entry) = inner.clusters.entry(cluster) {
            entry.insert(ClusterLeaf {
                value,
                leaf: Leaf {
                    read: false,
                    blessed: false,
                    until: 0,
                    chain_now: None,
                    ttl: 0,
                },
            });
            drop(inner);
            self.changed.notify_one();
        }
    }

    /// Note what the Solana cluster `cluster` says of the Solana key, as
    /// [`ServiceAttestation::note`] does for an EVM chain.
    pub fn note_cluster(
        &self,
        cluster: &str,
        blessed: bool,
        until: u64,
        chain_now: Option<u64>,
        ttl: u64,
    ) {
        let mut inner = self.inner();
        if let Some(served) = inner.clusters.get_mut(cluster) {
            let leaf = &mut served.leaf;
            let was_due = leaf.due();
            leaf.read = true;
            leaf.chain_now = chain_now;
            if chain_now.is_some() {
                leaf.blessed = blessed;
                leaf.until = until;
                leaf.ttl = ttl;
            }
            if leaf.due() && !was_due {
                drop(inner);
                self.changed.notify_one();
            }
        }
    }

    /// Whether the Solana key is blessed as the service's kind on the
    /// cluster `cluster`, as last read there: until then, no work is taken
    /// on that cluster.
    pub fn ready_cluster(&self, cluster: &str) -> bool {
        self.inner()
            .clusters
            .get(cluster)
            .is_some_and(|served| served.leaf.read && served.leaf.blessed)
    }

    /// Every leaf, EVM and Solana alike.
    fn every_leaf(inner: &Inner) -> impl Iterator<Item = &Leaf> {
        inner
            .leaves
            .values()
            .chain(inner.clusters.values().map(|served| &served.leaf))
    }

    /// When the service should next be running to renew its blessings: the
    /// soonest, over the chains served, that a blessing is down to a third
    /// of its length. `None` until some chain has said when its runs out.
    pub fn renew_at(&self) -> Option<u64> {
        let inner = self.inner();
        Self::every_leaf(&inner)
            .filter(|leaf| leaf.until > 0)
            .map(|leaf| leaf.until.saturating_sub(leaf.ttl / 3))
            .min()
    }

    /// Whether an attestation is in progress: some chain not yet read, or a
    /// leaf due and not yet renewed.
    pub fn busy(&self) -> bool {
        Self::every_leaf(&self.inner()).any(Leaf::due)
    }

    fn leaf_hashes(&self, inner: &Inner) -> Vec<(u64, B256)> {
        inner
            .leaves
            .keys()
            .map(|chain_id| (*chain_id, service_leaf(*chain_id, self.key)))
            .collect()
    }

    fn cluster_leaf_hashes(&self, inner: &Inner) -> Vec<(String, B256)> {
        inner
            .clusters
            .iter()
            .map(|(cluster, served)| {
                (
                    cluster.clone(),
                    solana_service_leaf(&served.value, &self.solana_key),
                )
            })
            .collect()
    }

    /// The tree's root over the leaves, the EVM chains' then the Solana
    /// clusters', and each leaf's proof: the EVM chains' in chain order,
    /// then the clusters' in theirs.
    #[allow(clippy::type_complexity)]
    fn tree(
        &self,
        inner: &Inner,
    ) -> (
        B256,
        Vec<(u64, B256, Vec<B256>)>,
        Vec<(String, B256, Vec<B256>)>,
    ) {
        let leaves = self.leaf_hashes(inner);
        let clusters = self.cluster_leaf_hashes(inner);
        let hashes: Vec<B256> = leaves
            .iter()
            .map(|(_, hash)| *hash)
            .chain(clusters.iter().map(|(_, hash)| *hash))
            .collect();
        let top = chain::attest::merkle_root(&hashes);
        let proofs = leaves
            .iter()
            .enumerate()
            .map(|(index, (chain_id, hash))| {
                (
                    *chain_id,
                    *hash,
                    chain::attest::merkle_proof(&hashes, index),
                )
            })
            .collect();
        let cluster_proofs = clusters
            .into_iter()
            .enumerate()
            .map(|(index, (cluster, hash))| {
                (
                    cluster,
                    hash,
                    chain::attest::merkle_proof(&hashes, leaves.len() + index),
                )
            })
            .collect();
        (top, proofs, cluster_proofs)
    }

    fn binding_of(&self, inner: &Inner) -> Binding {
        Binding {
            evm_signer: self.instance.into_array(),
            solana_signer: self.solana_instance.to_bytes(),
            vaults_root: self.tree(inner).0.0,
        }
    }

    /// What the instance's next token binds.
    pub fn binding(&self) -> Binding {
        self.binding_of(&self.inner())
    }

    pub fn status(&self) -> Status {
        let inner = self.inner();
        let binding = self.binding_of(&inner);
        let nonce = binding.nonce();
        let (_, proofs, cluster_proofs) = self.tree(&inner);
        let any_due = Self::every_leaf(&inner).any(Leaf::due);
        let token = inner.token.as_ref().filter(|held| held.nonce == nonce);
        let sealed = inner.sealed.as_ref().filter(|sealed| sealed.nonce == nonce);
        let hex = |bytes: &[u8]| alloy::hex::encode_prefixed(bytes);
        let seal_wanted = self.mode == Mode::Launcher
            && any_due
            && token.is_some_and(|held| {
                !sealed.is_some_and(|sealed| sealed.issued_at >= held.issued_at)
            });
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
            seal_wanted,
            nonce,
            token: token.map(|held| held.token.clone()),
            token_issued_at: token.map(|held| held.issued_at),
            seal: sealed.map(|sealed| hex(&sealed.seal)),
            journal: sealed.map(|sealed| hex(&sealed.journal)),
            leaves: proofs
                .into_iter()
                .map(|(chain_id, leaf, proof)| LeafView {
                    chain_id,
                    vault_chain: 0,
                    vault: Address::ZERO.to_string(),
                    strategy_hash: B256::ZERO.to_string(),
                    key: self.key.to_string(),
                    leaf: leaf.to_string(),
                    proof: proof.iter().map(ToString::to_string).collect(),
                })
                .collect(),
            root: None,
            solana: self.solana_views(&inner, sealed, cluster_proofs),
        }
    }

    /// Each cluster's leaf, and the journal and seal its registry takes for
    /// the current tree, rebuilt from the prover's seal. A seal naming
    /// another Solana key than the instance's is no seal for a cluster.
    fn solana_views(
        &self,
        inner: &Inner,
        sealed: Option<&Sealed>,
        proofs: Vec<(String, B256, Vec<B256>)>,
    ) -> Vec<SolanaView> {
        let hex = |bytes: &[u8]| alloy::hex::encode_prefixed(bytes);
        let solana = match self.mode {
            Mode::Launcher => sealed
                .and_then(|sealed| SolanaSeal::from_evm(&sealed.journal, &sealed.seal))
                .filter(|(journal, _)| journal.signer == self.solana_instance),
            Mode::Development => None,
        };
        proofs
            .into_iter()
            .map(|(cluster, leaf, proof)| {
                let due = inner
                    .clusters
                    .get(&cluster)
                    .is_some_and(|served| served.leaf.due());
                SolanaView {
                    cluster,
                    journal: solana.as_ref().map(|(journal, _)| {
                        hex(
                            &chain::solana::programs::anchor_lang::AnchorSerialize::try_to_vec(
                                journal,
                            )
                            .unwrap_or_default(),
                        )
                    }),
                    seal: solana.as_ref().map(|(_, seal)| seal.clone()),
                    leaves: vec![SolanaLeafView {
                        vault: Pubkey::default().to_string(),
                        strategy_hash: hex(&[0; 32]),
                        key: self.solana_key.to_string(),
                        due,
                        leaf: leaf.to_string(),
                        proof: proof.iter().map(ToString::to_string).collect(),
                    }],
                }
            })
            .collect()
    }

    /// The image and project a journal must name: the development ones, or
    /// those of the token held for `nonce`.
    fn expected_identity(&self, inner: &Inner, nonce: &str) -> Option<(B256, B256, Option<u64>)> {
        match self.mode {
            Mode::Development => Some((
                chain::attest::image_digest(development_name(self.kind)),
                chain::attest::project_id(),
                None,
            )),
            Mode::Launcher => inner
                .token
                .as_ref()
                .filter(|held| held.nonce == nonce)
                .map(|held| (held.image_digest, held.project_id, Some(held.issued_at))),
        }
    }

    /// Whether something is to be asked for now, by the wall clock
    /// `wall_now` and each chain's own as last read.
    fn wanted(&self, wall_now: u64) -> bool {
        let inner = self.inner();
        let wanting: Vec<Wanting> = Self::every_leaf(&inner)
            .filter(|leaf| leaf.read && leaf.due())
            .map(|leaf| Wanting {
                chain_now: leaf.chain_now,
                until: leaf.until,
                ttl: leaf.ttl,
            })
            .collect();
        let nonce = self.binding_of(&inner).nonce();
        let sealed = inner
            .sealed
            .as_ref()
            .map(|sealed| (sealed.nonce.as_str(), sealed.issued_at));
        let token = match self.mode {
            Mode::Launcher => inner
                .token
                .as_ref()
                .map(|held| (held.nonce.as_str(), held.issued_at)),
            Mode::Development => None,
        };
        token_wanted(&wanting, &nonce, token, sealed, wall_now)
    }

    /// Keep a token, if it is for the current tree.
    fn hold(&self, token: String) -> Result<()> {
        let claims = claims(&token)?;
        anyhow::ensure!(
            claims.audience == AUDIENCE,
            "the token is for another audience"
        );
        let mut inner = self.inner();
        let nonce = self.binding_of(&inner).nonce();
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
    /// what is left is the simulated `bless` on each chain, with its leaf's
    /// proof.
    #[allow(clippy::type_complexity)]
    fn check(
        &self,
        post: &SealPost,
    ) -> Result<(Sealed, Vec<Simulate>, Vec<SimulateCluster>), Refusal> {
        let inner = self.inner();
        let binding = self.binding_of(&inner);
        let nonce = binding.nonce();
        if post.nonce != nonce {
            return Err(Refusal::Nonce);
        }
        let refused = |why: &str| Refusal::Journal(why.to_string());
        if inner.leaves.is_empty() && inner.clusters.is_empty() {
            return Err(refused("the service serves no chain"));
        }
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
        if journal.signer != self.instance {
            return Err(refused("the journal names another instance"));
        }
        if journal.vaults_root != B256::from(binding.vaults_root) {
            return Err(refused("the journal is for another tree"));
        }
        let (image, project, issued_at) = self
            .expected_identity(&inner, &nonce)
            .ok_or_else(|| refused("no token is held for this tree"))?;
        if issued_at.is_some_and(|issued_at| issued_at != journal.issued_at) {
            return Err(refused("the journal is for another token"));
        }
        if journal.image_digest != image || journal.project_id != project {
            return Err(refused("the journal names another image or project"));
        }
        // A Solana registry reads the same proof with the instance's Solana
        // key after it: it has to be this instance's.
        if self.mode == Mode::Launcher
            && !inner.clusters.is_empty()
            && SolanaSeal::from_evm(&journal_bytes, &seal)
                .is_none_or(|(journal, _)| journal.signer != self.solana_instance)
        {
            return Err(refused("the seal names another Solana key"));
        }
        let (_, proofs, cluster_proofs) = self.tree(&inner);
        Ok((
            Sealed {
                nonce,
                seal,
                journal: journal_bytes,
                issued_at: journal.issued_at,
            },
            proofs
                .into_iter()
                .map(|(chain_id, _, proof)| (chain_id, proof))
                .collect(),
            cluster_proofs
                .into_iter()
                .map(|(cluster, _, proof)| (cluster, proof))
                .collect(),
        ))
    }

    /// Take a seal a prover posts: checked against the binding, then a
    /// `bless` of the key simulated on every chain served; kept only if each
    /// would land, so a chain whose checker refuses it doesn't wait on a seal
    /// that will never bless it there.
    ///
    /// A Solana cluster is asked through a far less reliable endpoint, and
    /// allows images and checkers through an admin path of its own: one that
    /// can't be asked, or refuses, holds back only its own blessing, which
    /// the keeper simulates again before it relays it, as the executor's
    /// does. The seal is refused over Solana only when it would bless the key
    /// nowhere at all.
    pub async fn accept(&self, post: SealPost) -> Result<(), Refusal> {
        let (sealed, simulate, clusters) = self.check(&post)?;
        let mut sealable = !simulate.is_empty();
        let mut cluster_refused = None;
        for (cluster, proof) in clusters {
            match self.simulate_cluster(&sealed, &cluster, &proof).await {
                Ok(()) => sealable = true,
                Err(refusal) => {
                    tracing::warn!(
                        cluster,
                        why = ?refusal,
                        "a simulated bless on this cluster did not go through; the key waits there for the keeper's own simulation or a later seal"
                    );
                    cluster_refused.get_or_insert(refusal);
                }
            }
        }
        if let Some(refusal) = cluster_refused
            && !sealable
        {
            return Err(refusal);
        }
        for (chain_id, proof) in simulate {
            let chain = self
                .chains
                .lock()
                .ok()
                .and_then(|chains| chains.get(&chain_id).cloned());
            let Some((client, deployments)) = chain else {
                return Err(Refusal::Unavailable(format!(
                    "chain {chain_id} is not connected"
                )));
            };
            let simulated = client
                .read(
                    deployments.attestation,
                    IAttestationRegistry::blessCall {
                        seal: sealed.seal.clone(),
                        journal: sealed.journal.clone(),
                        vaultChain: U256::ZERO,
                        vault: Address::ZERO,
                        strategyHash: B256::ZERO,
                        key: self.key,
                        merkleProof: proof,
                    },
                )
                .await;
            if let Err(error) = simulated {
                let why = format!("{error:#}");
                return Err(if why.contains("revert") {
                    Refusal::Journal(format!(
                        "a simulated bless reverts on chain {chain_id}: {why}"
                    ))
                } else {
                    Refusal::Unavailable(why)
                });
            }
        }
        self.keep(sealed)
    }

    /// Simulate a `bless` of the Solana key on `cluster` with the seal, as
    /// the keeper would send it there.
    async fn simulate_cluster(
        &self,
        sealed: &Sealed,
        cluster: &str,
        proof: &[B256],
    ) -> Result<(), Refusal> {
        let served = self
            .clusters
            .lock()
            .ok()
            .and_then(|clusters| clusters.get(cluster).cloned())
            .ok_or_else(|| Refusal::Unavailable(format!("{cluster} is not connected")))?;
        let (journal, seal) =
            SolanaSeal::from_evm(&sealed.journal, &sealed.seal).ok_or_else(|| {
                Refusal::Journal("the seal is not one the Solana checker reads".into())
            })?;
        let leaf = KeyLeaf {
            vault_chain: [0; 32],
            vault: [0; 32],
            strategy_hash: [0; 32],
            key: self.solana_key,
            proof: proof.iter().map(|hash| hash.0).collect(),
        };
        served
            .platform
            .simulate_bless(&served.client, &served.payer, &journal, &seal, &leaf)
            .await
            .map_err(|error| {
                let why = format!("{error:#}");
                if error
                    .downcast_ref::<chain::solana::TransactionFailed>()
                    .is_some()
                {
                    Refusal::Journal(format!("a simulated bless fails on {cluster}: {why}"))
                } else {
                    Refusal::Unavailable(why)
                }
            })
    }

    fn keep(&self, sealed: Sealed) -> Result<(), Refusal> {
        let mut inner = self.inner();
        if self.binding_of(&inner).nonce() != sealed.nonce {
            return Err(Refusal::Nonce);
        }
        tracing::info!(
            issued_at = sealed.issued_at,
            "a seal for the service's key is accepted"
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
        let mut head = u64::MAX;
        for (client, _) in &chains {
            head = head.min(client.block_timestamp().await?);
        }
        let issued_at = dated(head);
        let (nonce, root) = {
            let inner = self.inner();
            let binding = self.binding_of(&inner);
            (binding.nonce(), B256::from(binding.vaults_root))
        };
        let journal = Journal {
            image_digest: chain::attest::image_digest(development_name(self.kind)),
            project_id: chain::attest::project_id(),
            signer: self.instance,
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

    /// Read the key's standing on every chain served.
    async fn read_chains(&self) {
        let chains: Vec<(u64, Client, Deployments)> = self
            .chains
            .lock()
            .map(|chains| {
                chains
                    .iter()
                    .map(|(id, (client, deployments))| (*id, client.clone(), deployments.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let clusters: Vec<(String, ClusterServed)> = self
            .clusters
            .lock()
            .map(|clusters| {
                clusters
                    .iter()
                    .map(|(cluster, served)| (cluster.clone(), served.clone()))
                    .collect()
            })
            .unwrap_or_default();
        for (cluster, served) in clusters {
            match served
                .platform
                .service_standing(&served.client, &self.solana_key, self.kind.to_solana())
                .await
            {
                Ok(standing) => self.note_cluster(
                    &cluster,
                    standing.blessed,
                    standing.until,
                    Some(standing.now),
                    standing.ttl,
                ),
                Err(error) => {
                    tracing::debug!(
                        cluster,
                        error = format!("{error:#}"),
                        "could not read the service key's blessing"
                    );
                    self.note_cluster(&cluster, false, 0, None, 0);
                }
            }
        }
        for (chain_id, client, deployments) in chains {
            match read_standing(&client, &deployments, self.key, self.kind).await {
                Ok((blessed, until, now, ttl)) => {
                    self.note(chain_id, blessed, until, Some(now), ttl)
                }
                Err(error) => {
                    tracing::debug!(
                        chain_id,
                        error = format!("{error:#}"),
                        "could not read the service key's blessing"
                    );
                    self.note(chain_id, false, 0, None, 0);
                }
            }
        }
    }

    /// Read the chains, and ask for a token (or, in development mode, seal)
    /// whenever a blessing is due, for as long as the service runs.
    pub async fn run(self, socket: PathBuf) {
        let mut failures: u32 = 0;
        let mut next_try = Instant::now();
        let mut next_read = Instant::now();
        let mut said: Option<Instant> = None;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(TICK) => {}
                _ = self.changed.notified() => {
                    next_read = Instant::now();
                }
            }
            if Instant::now() >= next_read {
                self.read_chains().await;
                next_read = Instant::now() + READ_EVERY;
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
                    tracing::info!(key = %self.key, root = %B256::from(self.binding().vaults_root), "the service's key is bound in {what}");
                }
                Err(error) => {
                    failures += 1;
                    next_try = Instant::now() + backoff(failures);
                    if said.is_none_or(|said| said.elapsed() >= Duration::from_secs(60)) {
                        said = Some(Instant::now());
                        tracing::warn!(
                            error = format!("{error:#}"),
                            "could not attest the service's key; trying again"
                        );
                    }
                }
            }
        }
    }

    /// `GET /attestation` and `POST /attestation/seal`.
    pub fn routes(&self) -> axum::Router {
        let reading = self.clone();
        let accepting = self.clone();
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
                    let attestation = accepting.clone();
                    async move {
                        match attestation.accept(post).await {
                            Ok(()) => (StatusCode::NO_CONTENT, String::new()),
                            Err(refusal) => {
                                let why = match &refusal {
                                    Refusal::Nonce => "not the current binding's nonce".to_string(),
                                    Refusal::Journal(why) | Refusal::Unavailable(why) => {
                                        why.clone()
                                    }
                                };
                                tracing::info!(why, "a posted seal was not taken");
                                (refusal.status(), why)
                            }
                        }
                    }
                }),
            )
    }
}

/// How long to wait after the `failures`th failure in a row to attest: 5
/// seconds, doubling, at most five minutes.
fn backoff(failures: u32) -> Duration {
    Duration::from_secs((5u64 << failures.saturating_sub(1).min(6)).min(300))
}

/// The key's standing on one chain: whether it is blessed as `kind`, when
/// its blessing runs out, the chain's time and its blessing TTL.
async fn read_standing(
    client: &Client,
    deployments: &Deployments,
    key: Address,
    kind: ImageKind,
) -> Result<(bool, u64, u64, u64)> {
    let registry = deployments.attestation;
    let ttl = client
        .read(registry, IAttestationRegistry::blessingTtlCall {})
        .await?;
    let info = client
        .read(registry, IAttestationRegistry::keyInfoCall { signer: key })
        .await?;
    let blessed = client
        .read(
            registry,
            IAttestationRegistry::isBlessedAsCall {
                signer: key,
                kind: kind.to_chain(),
            },
        )
        .await?;
    let now = client.block_timestamp().await?;
    Ok((blessed, info.blessedUntil, now, ttl))
}

#[cfg(test)]
pub(super) mod tests_support {
    use super::*;

    /// Hold `token` as if the launcher had given it.
    pub fn hold(attestation: &ServiceAttestation, token: String) -> Result<()> {
        attestation.hold(token)
    }

    pub fn wanted(attestation: &ServiceAttestation, wall_now: u64) -> bool {
        attestation.wanted(wall_now)
    }

    /// Keep a seal as if every chain's simulation had passed.
    pub fn accept_unsimulated(
        attestation: &ServiceAttestation,
        post: &SealPost,
    ) -> Result<(), Refusal> {
        let (sealed, _, _) = attestation.check(post)?;
        attestation.keep(sealed)
    }

    pub fn journal(
        image_digest: B256,
        project_id: B256,
        signer: Address,
        vaults_root: B256,
        issued_at: u64,
    ) -> Vec<u8> {
        Journal {
            image_digest,
            project_id,
            signer,
            vaults_root,
            issued_at,
        }
        .encode()
    }
}
