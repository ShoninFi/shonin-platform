//! What the holder asks the canonical chain before releasing anything.
//!
//! The chain is the only authority on which images may receive which keys:
//! adding an image, a project or a checker is timelocked and public there.
//! [`ChainView`] is what the exchange reads; [`RegistryView`] reads it from
//! the canonical chain's `AttestationRegistry`, and tests use a stand-in.

use std::fmt;
use std::future::Future;
use std::path::Path;

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

/// The canonical chain, as the exchange reads it. Every answer is as of
/// now; an error means nothing could be decided, and nothing is released.
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

    /// Whether `key` is blessed for the vault and bundle now.
    fn is_blessed_for(
        &self,
        key: Address,
        vault_chain: u64,
        vault: Address,
        strategy_hash: B256,
    ) -> impl Future<Output = Result<bool>> + Send;

    /// The image `key` was last blessed from, if it has been.
    fn key_image(&self, key: Address) -> impl Future<Output = Result<Option<B256>>> + Send;

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

/// The canonical chain's `AttestationRegistry`, read through a client.
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

    async fn key_image(&self, key: Address) -> Result<Option<B256>> {
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
