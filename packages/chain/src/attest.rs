//! Proving what a service is, and getting its key blessed.
//!
//! On a real deployment the enclave produces a proof no service could forge,
//! and the registry checks it against the platform's own trust root. On a
//! development chain a verifier contract trusts one account's signature over
//! the same journal instead. Everything downstream is the same either way:
//! which images may hold a key, how long an addition waits, what a key may do
//! and for how long. The trust root says which of the two is in use, and every
//! vault's page shows it.

use std::time::Duration;

use alloy::primitives::{Address, B256, keccak256};
use alloy::sol_types::SolValue;
use anyhow::{Context, Result, bail};

use crate::bindings::attestation::{IAttestationRegistry, ImageKind, KeyInfo};
use crate::client::{Client, KeyClient};
use crate::deployments::Deployments;

/// Who puts a blessing on the chain. Anyone may — the proof is what the
/// registry checks — but a key that holds its own account sends with it, so
/// two services never reach for the same account's next nonce at once.
#[derive(Clone)]
pub enum Sender {
    /// An account the development chain itself holds the key for.
    Unlocked(Address),
    /// A key made inside the service, which signs its own transactions.
    Own(KeyClient),
}

impl Sender {
    async fn send<C: alloy::sol_types::SolCall>(
        &self,
        chain: &Client,
        to: Address,
        call: C,
    ) -> Result<()> {
        match self {
            Sender::Unlocked(from) => chain.send(*from, to, call).await.map(|_| ()),
            Sender::Own(key) => key.send(to, call).await.map(|_| ()),
        }
    }
}

/// What an instance attests to. It matches `AttestationRegistry.Journal`.
type Journal = (B256, B256, Address, B256);

/// What a service's image is. There is no enclave here to measure, so the
/// service's name stands for its digest and the registry treats it exactly as
/// it would a real one.
pub fn image_digest(service: &str) -> B256 {
    keccak256(format!("shonin-dev-image:{service}"))
}

pub fn project_id() -> B256 {
    keccak256("shonin-dev-project")
}

/// Bless a key that serves every strategy and no particular vault: a build
/// service, or a reviewer.
///
/// The key sends its own blessing. Anyone may send one — the proof is what
/// the registry checks — but having each key send its own keeps two services
/// from reaching for the same account's next nonce at the same moment.
pub async fn bless_service(
    chain: &Client,
    deployments: &Deployments,
    service: &str,
    key: Address,
) -> Result<KeyInfo> {
    bless(
        chain,
        deployments,
        service,
        key,
        Sender::Unlocked(key),
        Address::ZERO,
        B256::ZERO,
    )
    .await
}

/// Bless a key for one vault running one bundle, sent by `sender`.
pub async fn bless_for_vault(
    chain: &Client,
    deployments: &Deployments,
    service: &str,
    key: Address,
    sender: Sender,
    vault: Address,
    bundle: B256,
) -> Result<KeyInfo> {
    bless(chain, deployments, service, key, sender, vault, bundle).await
}

#[allow(clippy::too_many_arguments)]
async fn bless(
    chain: &Client,
    deployments: &Deployments,
    service: &str,
    key: Address,
    sender: Sender,
    vault: Address,
    bundle: B256,
) -> Result<KeyInfo> {
    let image = image_digest(service);
    // The one pair this instance holds a key for. A larger instance would
    // commit to a root over many and prove one of them here.
    let root = if vault == Address::ZERO {
        B256::ZERO
    } else {
        keccak256(keccak256((vault, bundle).abi_encode()))
    };
    let journal: Journal = (image, project_id(), key, root);
    let encoded = journal.abi_encode_params();
    let seal = chain
        .sign(deployments.roles.attestor, keccak256(&encoded))
        .await
        .context("the attestation was not sealed")?;

    // An image waits out its timelock before it can hold a key. A service
    // that starts the moment the chain does is simply early, so it waits
    // rather than gives up.
    let mut waited = Duration::ZERO;
    loop {
        let allowed = chain
            .read(
                deployments.attestation,
                IAttestationRegistry::isImageAllowedCall { imageDigest: image },
            )
            .await
            .unwrap_or(false);

        if allowed {
            sender
                .send(
                    chain,
                    deployments.attestation,
                    IAttestationRegistry::blessCall {
                        seal: seal.clone(),
                        journal: encoded.clone().into(),
                        vault,
                        strategyHash: bundle,
                        merkleProof: Vec::new(),
                    },
                )
                .await
                .context("the registry would not bless the key")?;
            break;
        }

        if waited > Duration::from_secs(300) {
            bail!("the {service} image is still not allowlisted");
        }
        if waited.is_zero() {
            tracing::info!(service, "waiting out the image's timelock");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        waited += Duration::from_secs(2);
    }

    chain
        .read(
            deployments.attestation,
            IAttestationRegistry::keyInfoCall { signer: key },
        )
        .await
        .context("reading back what was blessed")
}

/// Keep a key blessed for as long as this service runs.
///
/// A blessing expires, and an expired key is exactly as useless as a revoked
/// one — which is the point: an instance that stops attesting stops being
/// able to act. So an instance that is still running attests again well
/// before its blessing runs out.
#[allow(clippy::too_many_arguments)]
pub fn keep_blessed(
    chain: Client,
    deployments: Deployments,
    service: &'static str,
    key: Address,
    sender: Sender,
    vault: Address,
    bundle: B256,
    mut on_renewal: impl FnMut() + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let ttl = chain
                .read(
                    deployments.attestation,
                    IAttestationRegistry::blessingTtlCall {},
                )
                .await
                .unwrap_or(72 * 3_600);

            // Renew with a third of the blessing left, so a slow chain or a
            // busy moment never leaves a gap.
            let info = chain
                .read(
                    deployments.attestation,
                    IAttestationRegistry::keyInfoCall { signer: key },
                )
                .await
                .ok();
            let now = chain.block_timestamp().await.unwrap_or_default();
            let left = info
                .map(|info| info.blessedUntil.saturating_sub(now))
                .unwrap_or(0);

            if left * 3 <= ttl {
                match bless(
                    &chain,
                    &deployments,
                    service,
                    key,
                    sender.clone(),
                    vault,
                    bundle,
                )
                .await
                {
                    Ok(_) => on_renewal(),
                    // A key that stops being blessed stops being able to act,
                    // so this is never a quiet failure.
                    Err(error) => tracing::warn!(service, %error, "could not renew a blessing"),
                }
            }

            // The chain's clock runs at its own speed, so wait in its terms.
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}

/// Which kind of image a service runs as, for the pages that say so.
pub fn kind_of(service: &str) -> ImageKind {
    match service {
        "builder" => ImageKind::Builder,
        "reviewer" => ImageKind::Reviewer,
        "unwinder" => ImageKind::Unwinder,
        _ => ImageKind::Executor,
    }
}
