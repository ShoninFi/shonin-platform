//! What the holder asks the chains before releasing anything.
//!
//! The canonical chain is the only authority on which images may receive
//! which keys: adding an image, a project or a checker is timelocked and
//! public there. A vault key, though, is blessed on its vault's own chain,
//! so whether a key is blessed for a vault, and from which image, is read
//! from the registry on that vault's chain, and never from another.
//! [`ChainView`] is what the exchange reads; [`RegistryView`] reads one
//! chain's `AttestationRegistry`, [`NetworkView`] puts the canonical chain's
//! together with each vault chain's, and tests use a stand-in.
//!
//! One decision reads each chain at one block ([`ChainView::pin`]): its head
//! when the decision begins, and never a block older than one an earlier
//! decision read that chain at. An RPC name can front several nodes, some
//! blocks behind the others; read unpinned, a node behind could answer one
//! request with a revocation already undone by its older view, after the
//! holder had refused on the newer one. Pinned, a node that hasn't reached
//! the block says so, and the read goes elsewhere or fails.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use alloy::primitives::{Address, B256, U256};
use anyhow::{Context, Result};
use chain::bindings::attestation::{
    IAttestationRegistry, ImageKind as ChainImageKind, RootKeyRecord,
};
use chain::{ChainConfigs, ChainKey, Client, Deployments, Signing};
use serde::{Deserialize, Serialize};

/// What an allowed image is for, as the registry lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageKind {
    Executor,
    Unwinder,
    Builder,
    Reviewer,
    Holder,
}

impl fmt::Display for ImageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ImageKind::Executor => "an executor",
            ImageKind::Unwinder => "an unwinder",
            ImageKind::Builder => "a builder",
            ImageKind::Reviewer => "a reviewer",
            ImageKind::Holder => "a holder",
        })
    }
}

impl ImageKind {
    /// The kind's value in the registry's `ImageKind` (`Executor` is 1,
    /// `Holder` 5).
    pub fn to_byte(self) -> u8 {
        match self {
            ImageKind::Executor => 1,
            ImageKind::Unwinder => 2,
            ImageKind::Builder => 3,
            ImageKind::Reviewer => 4,
            ImageKind::Holder => 5,
        }
    }

    /// The kind as the registry's bindings spell it.
    pub fn to_chain(self) -> ChainImageKind {
        match self {
            ImageKind::Executor => ChainImageKind::Executor,
            ImageKind::Unwinder => ChainImageKind::Unwinder,
            ImageKind::Builder => ChainImageKind::Builder,
            ImageKind::Reviewer => ChainImageKind::Reviewer,
            ImageKind::Holder => ChainImageKind::Holder,
        }
    }

    fn from_chain(kind: ChainImageKind) -> Option<ImageKind> {
        match kind {
            ChainImageKind::Executor => Some(ImageKind::Executor),
            ChainImageKind::Unwinder => Some(ImageKind::Unwinder),
            ChainImageKind::Builder => Some(ImageKind::Builder),
            ChainImageKind::Reviewer => Some(ImageKind::Reviewer),
            ChainImageKind::Holder => Some(ImageKind::Holder),
            _ => None,
        }
    }
}

/// The chains, as the exchange reads them: the canonical chain for who may
/// have what, and a vault's own chain for its blessings. Every answer is as
/// of the block `at` that [`ChainView::pin`] gave for that chain; an error
/// means nothing could be decided, and nothing is released.
pub trait ChainView: Send + Sync {
    /// The canonical chain's id, which published keys are signed for.
    fn chain_id(&self) -> u64;

    /// The block one decision reads chain `chain` at: its head now, or the
    /// newest block an earlier decision read it at, whichever is newer, so
    /// no decision is ever made on an older view of a chain than one already
    /// acted on.
    fn pin(&self, chain: u64) -> impl Future<Output = Result<u64>> + Send;

    /// The Google signing keys trusted, by their key hash
    /// (`Jwk::key_hash`), by any checker the canonical registry allows at
    /// block `at`.
    fn trusted_google_keys(&self, at: u64) -> impl Future<Output = Result<Vec<[u8; 32]>>> + Send;

    /// The kind an image is allowed as at block `at` of the canonical chain,
    /// or `None` if it is not allowed then (never proposed, still
    /// timelocked, or revoked).
    fn image_kind(
        &self,
        image_digest: B256,
        at: u64,
    ) -> impl Future<Output = Result<Option<ImageKind>>> + Send;

    /// Whether the operator project is allowed at block `at` of the
    /// canonical chain.
    fn project_allowed(
        &self,
        project_id: B256,
        at: u64,
    ) -> impl Future<Output = Result<bool>> + Send;

    /// Whether the registry on chain `chain` is read here. A vault on a
    /// chain that isn't has its keys refused: its blessings are never read
    /// from another chain's registry.
    fn reads_chain(&self, chain: u64) -> bool;

    /// Whether `key` is blessed for the vault and bundle at block `at`,
    /// read from the registry on the vault's own chain, `vault_chain`.
    fn is_blessed_for(
        &self,
        key: Address,
        vault_chain: u64,
        vault: Address,
        strategy_hash: B256,
        at: u64,
    ) -> impl Future<Output = Result<bool>> + Send;

    /// The image `key` was last blessed from, if it has been, read from the
    /// registry on chain `chain` at block `at`.
    fn key_image(
        &self,
        chain: u64,
        key: Address,
        at: u64,
    ) -> impl Future<Output = Result<Option<B256>>> + Send;

    /// The root key's recorded signer while the record is live
    /// (`isRootKeyLive()`) at block `at` of the canonical chain, or `None`:
    /// none recorded, or cleared, or its image, project or checker no longer
    /// allowed.
    fn live_root_signer(&self, at: u64) -> impl Future<Output = Result<Option<Address>>> + Send;
}

alloy::sol! {
    /// What a Confidential Space checker says of the Google keys it trusts.
    interface IGoogleKeys {
        function trustedKeys() external view returns (bytes32[] memory);
    }
}

/// One chain's `AttestationRegistry`, read through a client. As a
/// [`ChainView`] it reads that chain alone: for the canonical chain's own
/// vaults, and for anything that asks only the canonical chain.
#[derive(Clone)]
pub struct RegistryView {
    client: Client,
    registry: Address,
    chain_id: u64,
    /// The newest block a decision read this chain at, shared by every
    /// clone: no later decision reads an older one.
    newest: Arc<AtomicU64>,
}

impl RegistryView {
    pub fn new(client: Client, registry: Address, chain_id: u64) -> RegistryView {
        RegistryView {
            client,
            registry,
            chain_id,
            newest: Arc::new(AtomicU64::new(0)),
        }
    }

    /// A read of the registry (or of a checker it lists) as of block `at`.
    ///
    /// A read some node answered at `at` makes `at` the newest block read
    /// here: a head only counts once a node has served a read at it.
    async fn read_at<C: alloy::sol_types::SolCall>(
        &self,
        to: Address,
        call: C,
        at: u64,
    ) -> Result<C::Return> {
        let returned = self
            .client
            .read_at(to, call, alloy::eips::BlockId::number(at))
            .await?;
        self.newest.fetch_max(at, Ordering::SeqCst);
        Ok(returned)
    }

    /// Connect, read-only, to the EVM chain `chain` of `chains.toml`, with
    /// its registry from the deployment file in `deployments_dir`.
    pub async fn connect(
        chains_toml: &Path,
        chain: &ChainKey,
        deployments_dir: &Path,
    ) -> Result<RegistryView> {
        let configs = ChainConfigs::load(chains_toml)?;
        let config = configs
            .get(chain)
            .with_context(|| format!("{chain} is not in {}", chains_toml.display()))?;
        anyhow::ensure!(
            chain.evm_chain_id().is_some(),
            "the canonical chain must be an EVM chain, not {chain}"
        );
        let client = Client::connect(config, Signing::ReadOnly).await?;
        let deployments: Deployments = client
            .deployments(&Deployments::file_in(deployments_dir, chain))
            .await?;
        Ok(RegistryView::new(
            client,
            deployments.attestation,
            deployments.chain_id,
        ))
    }

    /// The root key's record, whether or not it is live.
    pub async fn root_key(&self) -> Result<RootKeyRecord> {
        self.client
            .read(self.registry, IAttestationRegistry::rootKeyCall {})
            .await
    }

    /// Whether the record is set and its image, project and checker are
    /// all still allowed.
    pub async fn is_root_key_live(&self) -> Result<bool> {
        self.client
            .read(self.registry, IAttestationRegistry::isRootKeyLiveCall {})
            .await
    }
}

/// Whether a read of a checker's Google keys failed because the checker
/// names none: the call reverted (it has no such function), or what it
/// returned is not a list of keys. Any other failure is the read not being
/// answered.
fn names_no_keys(error: &anyhow::Error) -> bool {
    use alloy::transports::{RpcError, TransportErrorKind};
    error.chain().any(|cause| {
        cause.downcast_ref::<alloy::sol_types::Error>().is_some()
            || matches!(
                cause.downcast_ref::<RpcError<TransportErrorKind>>(),
                Some(RpcError::ErrorResp(refusal))
                    if refusal.code == 3
                        || refusal.message.to_ascii_lowercase().contains("revert")
            )
    })
}

impl ChainView for RegistryView {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    fn reads_chain(&self, chain: u64) -> bool {
        chain == self.chain_id
    }

    async fn pin(&self, chain: u64) -> Result<u64> {
        anyhow::ensure!(
            chain == self.chain_id,
            "chain {chain}'s registry is not read here"
        );
        let head = self.client.block_number().await?;
        // A node behind the others answers with an older head: the decision
        // is read at the newest block already read at instead, which such a
        // node hasn't reached, so it never answers it. The head said is not
        // the floor until a read at it is answered ([`Self::read_at`]): a
        // head no node can serve fails this decision, not every later one.
        Ok(head.max(self.newest.load(Ordering::SeqCst)))
    }

    async fn trusted_google_keys(&self, at: u64) -> Result<Vec<[u8; 32]>> {
        let verifiers = self
            .read_at(self.registry, IAttestationRegistry::verifiersCall {}, at)
            .await?;
        let mut trusted = Vec::new();
        for verifier in verifiers {
            let allowed = self
                .read_at(
                    self.registry,
                    IAttestationRegistry::isVerifierAllowedCall { verifier },
                    at,
                )
                .await?;
            if !allowed {
                continue;
            }
            // A checker that names no Google keys (the development checker,
            // which trusts a signature instead) adds none; a read no node
            // answered fails the decision.
            match self
                .read_at(verifier, IGoogleKeys::trustedKeysCall {}, at)
                .await
            {
                Ok(keys) => trusted.extend(keys.into_iter().map(|key| key.0)),
                Err(error) if names_no_keys(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(trusted)
    }

    async fn image_kind(&self, image_digest: B256, at: u64) -> Result<Option<ImageKind>> {
        let allowed = self
            .read_at(
                self.registry,
                IAttestationRegistry::isImageAllowedCall {
                    imageDigest: image_digest,
                },
                at,
            )
            .await?;
        if !allowed {
            return Ok(None);
        }
        let image = self
            .read_at(
                self.registry,
                IAttestationRegistry::imageOfCall {
                    imageDigest: image_digest,
                },
                at,
            )
            .await?;
        Ok(ImageKind::from_chain(image.kind))
    }

    async fn project_allowed(&self, project_id: B256, at: u64) -> Result<bool> {
        self.read_at(
            self.registry,
            IAttestationRegistry::isProjectAllowedCall {
                projectId: project_id,
            },
            at,
        )
        .await
    }

    async fn is_blessed_for(
        &self,
        key: Address,
        vault_chain: u64,
        vault: Address,
        strategy_hash: B256,
        at: u64,
    ) -> Result<bool> {
        anyhow::ensure!(
            vault_chain == self.chain_id,
            "chain {vault_chain}'s registry is not read here"
        );
        self.read_at(
            self.registry,
            IAttestationRegistry::isBlessedForCall {
                signer: key,
                vaultChain: U256::from(vault_chain),
                vault,
                strategyHash: strategy_hash,
            },
            at,
        )
        .await
    }

    async fn key_image(&self, chain: u64, key: Address, at: u64) -> Result<Option<B256>> {
        anyhow::ensure!(
            chain == self.chain_id,
            "chain {chain}'s registry is not read here"
        );
        let info = self
            .read_at(
                self.registry,
                IAttestationRegistry::keyInfoCall { signer: key },
                at,
            )
            .await?;
        Ok((info.imageDigest != B256::ZERO).then_some(info.imageDigest))
    }

    async fn live_root_signer(&self, at: u64) -> Result<Option<Address>> {
        // Both read at the one block, so they agree.
        if !self
            .read_at(
                self.registry,
                IAttestationRegistry::isRootKeyLiveCall {},
                at,
            )
            .await?
        {
            return Ok(None);
        }
        let signer = self
            .read_at(self.registry, IAttestationRegistry::rootKeyCall {}, at)
            .await?
            .signer;
        Ok((signer != Address::ZERO).then_some(signer))
    }
}

/// The network's registries as a holder reads them: the canonical chain's
/// for the asker's image, project and the checkers' Google keys, and each
/// vault chain's for the blessings of its own vaults. Each chain's registry
/// is added as its chain is connected; until the canonical chain's is, every
/// read fails and nothing is released, and a vault on a chain whose registry
/// was never added has its keys refused.
#[derive(Clone)]
pub struct NetworkView {
    chain_id: u64,
    registries: Arc<RwLock<HashMap<u64, RegistryView>>>,
}

impl NetworkView {
    /// The network whose canonical chain is `chain_id`, with no registry
    /// connected yet.
    pub fn new(chain_id: u64) -> NetworkView {
        NetworkView {
            chain_id,
            registries: Default::default(),
        }
    }

    /// Read `registry` for its chain from now on.
    pub fn connect(&self, registry: RegistryView) {
        self.registries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(registry.chain_id, registry);
    }

    /// The registry on chain `chain`, if it is connected.
    fn on(&self, chain: u64) -> Result<RegistryView> {
        self.registries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&chain)
            .cloned()
            .with_context(|| format!("chain {chain} is not connected"))
    }

    fn canonical(&self) -> Result<RegistryView> {
        self.on(self.chain_id)
    }
}

impl ChainView for NetworkView {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    fn reads_chain(&self, chain: u64) -> bool {
        self.registries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(&chain)
    }

    async fn pin(&self, chain: u64) -> Result<u64> {
        self.on(chain)?.pin(chain).await
    }

    async fn trusted_google_keys(&self, at: u64) -> Result<Vec<[u8; 32]>> {
        self.canonical()?.trusted_google_keys(at).await
    }

    async fn image_kind(&self, image_digest: B256, at: u64) -> Result<Option<ImageKind>> {
        self.canonical()?.image_kind(image_digest, at).await
    }

    async fn project_allowed(&self, project_id: B256, at: u64) -> Result<bool> {
        self.canonical()?.project_allowed(project_id, at).await
    }

    async fn is_blessed_for(
        &self,
        key: Address,
        vault_chain: u64,
        vault: Address,
        strategy_hash: B256,
        at: u64,
    ) -> Result<bool> {
        self.on(vault_chain)?
            .is_blessed_for(key, vault_chain, vault, strategy_hash, at)
            .await
    }

    async fn key_image(&self, chain: u64, key: Address, at: u64) -> Result<Option<B256>> {
        self.on(chain)?.key_image(chain, key, at).await
    }

    async fn live_root_signer(&self, at: u64) -> Result<Option<Address>> {
        self.canonical()?.live_root_signer(at).await
    }
}
