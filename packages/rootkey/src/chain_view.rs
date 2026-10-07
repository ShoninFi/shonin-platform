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

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::path::Path;
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
/// of now; an error means nothing could be decided, and nothing is released.
pub trait ChainView: Send + Sync {
    /// The canonical chain's id, which published keys are signed for.
    fn chain_id(&self) -> u64;

    /// The Google signing keys trusted, by their key hash
    /// (`Jwk::key_hash`), by any checker the registry allows now.
    fn trusted_google_keys(&self) -> impl Future<Output = Result<Vec<[u8; 32]>>> + Send;

    /// The kind an image is allowed as, or `None` if it is not allowed now
    /// (never proposed, still timelocked, or revoked).
    fn image_kind(
        &self,
        image_digest: B256,
    ) -> impl Future<Output = Result<Option<ImageKind>>> + Send;

    /// Whether the operator project is allowed now.
    fn project_allowed(&self, project_id: B256) -> impl Future<Output = Result<bool>> + Send;

    /// Whether the registry on chain `chain` is read here. A vault on a
    /// chain that isn't has its keys refused: its blessings are never read
    /// from another chain's registry.
    fn reads_chain(&self, chain: u64) -> bool;

    /// Whether `key` is blessed for the vault and bundle now, read from the
    /// registry on the vault's own chain, `vault_chain`.
    fn is_blessed_for(
        &self,
        key: Address,
        vault_chain: u64,
        vault: Address,
        strategy_hash: B256,
    ) -> impl Future<Output = Result<bool>> + Send;

    /// The image `key` was last blessed from, if it has been, read from the
    /// registry on chain `chain`.
    fn key_image(
        &self,
        chain: u64,
        key: Address,
    ) -> impl Future<Output = Result<Option<B256>>> + Send;

    /// The root key's recorded signer while the record is live
    /// (`isRootKeyLive()`), or `None`: none recorded, or cleared, or its
    /// image, project or checker no longer allowed.
    fn live_root_signer(&self) -> impl Future<Output = Result<Option<Address>>> + Send;
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
}

impl RegistryView {
    pub fn new(client: Client, registry: Address, chain_id: u64) -> RegistryView {
        RegistryView {
            client,
            registry,
            chain_id,
        }
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

impl ChainView for RegistryView {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    fn reads_chain(&self, chain: u64) -> bool {
        chain == self.chain_id
    }

    async fn trusted_google_keys(&self) -> Result<Vec<[u8; 32]>> {
        let verifiers = self
            .client
            .read(self.registry, IAttestationRegistry::verifiersCall {})
            .await?;
        let mut trusted = Vec::new();
        for verifier in verifiers {
            let allowed = self
                .client
                .read(
                    self.registry,
                    IAttestationRegistry::isVerifierAllowedCall { verifier },
                )
                .await?;
            if !allowed {
                continue;
            }
            // A checker that names no Google keys (the development checker,
            // which trusts a signature instead) adds none.
            if let Ok(keys) = self
                .client
                .read(verifier, IGoogleKeys::trustedKeysCall {})
                .await
            {
                trusted.extend(keys.into_iter().map(|key| key.0));
            }
        }
        Ok(trusted)
    }

    async fn image_kind(&self, image_digest: B256) -> Result<Option<ImageKind>> {
        let allowed = self
            .client
            .read(
                self.registry,
                IAttestationRegistry::isImageAllowedCall {
                    imageDigest: image_digest,
                },
            )
            .await?;
        if !allowed {
            return Ok(None);
        }
        let image = self
            .client
            .read(
                self.registry,
                IAttestationRegistry::imageOfCall {
                    imageDigest: image_digest,
                },
            )
            .await?;
        Ok(ImageKind::from_chain(image.kind))
    }

    async fn project_allowed(&self, project_id: B256) -> Result<bool> {
        self.client
            .read(
                self.registry,
                IAttestationRegistry::isProjectAllowedCall {
                    projectId: project_id,
                },
            )
            .await
    }

    async fn is_blessed_for(
        &self,
        key: Address,
        vault_chain: u64,
        vault: Address,
        strategy_hash: B256,
    ) -> Result<bool> {
        anyhow::ensure!(
            vault_chain == self.chain_id,
            "chain {vault_chain}'s registry is not read here"
        );
        self.client
            .read(
                self.registry,
                IAttestationRegistry::isBlessedForCall {
                    signer: key,
                    vaultChain: U256::from(vault_chain),
                    vault,
                    strategyHash: strategy_hash,
                },
            )
            .await
    }

    async fn key_image(&self, chain: u64, key: Address) -> Result<Option<B256>> {
        anyhow::ensure!(
            chain == self.chain_id,
            "chain {chain}'s registry is not read here"
        );
        let info = self
            .client
            .read(
                self.registry,
                IAttestationRegistry::keyInfoCall { signer: key },
            )
            .await?;
        Ok((info.imageDigest != B256::ZERO).then_some(info.imageDigest))
    }

    async fn live_root_signer(&self) -> Result<Option<Address>> {
        // Liveness first: a record cleared between the two reads reads as
        // no signer, and one set again since names another.
        if !self.is_root_key_live().await? {
            return Ok(None);
        }
        let signer = self.root_key().await?.signer;
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

    async fn trusted_google_keys(&self) -> Result<Vec<[u8; 32]>> {
        self.canonical()?.trusted_google_keys().await
    }

    async fn image_kind(&self, image_digest: B256) -> Result<Option<ImageKind>> {
        self.canonical()?.image_kind(image_digest).await
    }

    async fn project_allowed(&self, project_id: B256) -> Result<bool> {
        self.canonical()?.project_allowed(project_id).await
    }

    async fn is_blessed_for(
        &self,
        key: Address,
        vault_chain: u64,
        vault: Address,
        strategy_hash: B256,
    ) -> Result<bool> {
        self.on(vault_chain)?
            .is_blessed_for(key, vault_chain, vault, strategy_hash)
            .await
    }

    async fn key_image(&self, chain: u64, key: Address) -> Result<Option<B256>> {
        self.on(chain)?.key_image(chain, key).await
    }

    async fn live_root_signer(&self) -> Result<Option<Address>> {
        self.canonical()?.live_root_signer().await
    }
}
