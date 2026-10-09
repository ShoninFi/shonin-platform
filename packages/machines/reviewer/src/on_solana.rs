//! A builder's or a reviewer's work on a Solana cluster: its own key there,
//! blessed and stocked before any work on the cluster is taken.
//!
//! In the attested image the key is the service's Solana key, derived for
//! its image by the key holder: its leaf joins the instance's attestation,
//! the keeper relays its blessing once the prover has proved the token, and
//! stocks it with lamports once the registry counts it. Nothing on the
//! cluster is taken until the registry counts the key as the service's kind
//! and it holds lamports for its sends. Elsewhere (a development cluster),
//! the key is the role key the cluster's development keys name, blessed by
//! the development attestor and kept blessed.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chain::solana::{Keypair, Pubkey, Signer as _};
use chain::{Role, SolanaServed};

use crate::cloud::Attesting;

/// A Solana cluster a service works on, and the key it signs with there.
pub struct OnSolana {
    pub served: SolanaServed,
    pub key: Arc<Keypair>,
}

impl OnSolana {
    /// The cluster's chain key (`solana:devnet`), as the attestation and the
    /// keeper name it.
    pub fn cluster(&self) -> String {
        self.served.key.to_string()
    }

    pub fn address(&self) -> Pubkey {
        self.key.pubkey()
    }
}

/// What a service key must hold before work is taken on a public cluster:
/// a few registrations' or reviews' rent and fees. The keeper keeps it at
/// more than this once the key is blessed.
pub const WORK_LAMPORTS_PUBLIC: u64 = 20_000_000;
/// And on a development cluster, where the role key is funded at set-up.
pub const WORK_LAMPORTS_DEVELOPMENT: u64 = 100_000_000;

/// What a service key must hold before work is taken on `cluster`.
pub fn work_lamports(cluster: &str) -> u64 {
    if cluster == "localnet" {
        WORK_LAMPORTS_DEVELOPMENT
    } else {
        WORK_LAMPORTS_PUBLIC
    }
}

/// Whether a key holding `holds` lamports may take work on `cluster`, blessed
/// or not: without them a paid job could fail for want of its rent.
pub fn stocked(cluster: &str, holds: u64) -> bool {
    holds >= work_lamports(cluster)
}

/// The role key a service of `kind` holds on a development cluster.
fn role_of(kind: shonin_rootkey::ImageKind) -> Result<Role> {
    match kind {
        shonin_rootkey::ImageKind::Builder => Ok(Role::Builder),
        shonin_rootkey::ImageKind::Reviewer => Ok(Role::Reviewer),
        other => Err(anyhow!("{other} is no service")),
    }
}

/// The development image's name for a service of `kind`.
fn service_name(kind: shonin_rootkey::ImageKind) -> &'static str {
    match kind {
        shonin_rootkey::ImageKind::Reviewer => "reviewer",
        _ => "builder",
    }
}

/// Take a cluster on: in the attested image, its leaf added to the
/// attestation and nothing returned until the registry counts the key and it
/// holds its lamports; elsewhere, the role key blessed and kept blessed.
/// `key` is the service's own Solana key, in the attested image.
pub async fn take_on(
    kind: shonin_rootkey::ImageKind,
    attestation: Option<Arc<dyn Attesting>>,
    key: Option<Arc<Keypair>>,
    served: SolanaServed,
    look_every: Duration,
) -> Result<OnSolana> {
    let cluster = served.key.to_string();
    match (attestation, key) {
        (Some(attestation), Some(key)) => {
            // A simulated blessing is paid for by the keeper, which sends it.
            let payer: Pubkey = served
                .deployments
                .roles
                .as_ref()
                .and_then(|roles| roles.keeper.parse().ok())
                .context("the cluster's deployment names no keeper to relay the blessing")?;
            attestation.serve_cluster(
                cluster.clone(),
                shonin_rootkey::service::ClusterServed {
                    client: served.client.clone(),
                    platform: served.platform.clone(),
                    payer,
                },
            );
            let mut said = false;
            loop {
                let blessed = attestation.ready_cluster(&cluster);
                let holds = if blessed {
                    served.client.lamports(&key.pubkey()).await.unwrap_or(0)
                } else {
                    0
                };
                if blessed && stocked(&served.client.cluster, holds) {
                    break;
                }
                if !said {
                    tracing::info!(%cluster, key = %key.pubkey(), blessed, "waiting for the Solana key's blessing and lamports");
                    said = true;
                }
                tokio::time::sleep(look_every).await;
            }
            Ok(OnSolana { served, key })
        }
        _ => {
            let role = role_of(kind)?;
            let key = Arc::new(
                served
                    .client
                    .role(role)
                    .with_context(|| {
                        format!("a development service on {cluster} signs with its role key")
                    })?
                    .insecure_clone(),
            );
            bless_development(kind, &served, &key).await?;
            let on = OnSolana { served, key };
            keep_blessed_development(kind, on.served.clone(), on.key.clone());
            Ok(on)
        }
    }
}

/// Bless the role key on a development cluster with the development
/// attestor's seal.
async fn bless_development(
    kind: shonin_rootkey::ImageKind,
    served: &SolanaServed,
    key: &Keypair,
) -> Result<()> {
    let attestor = served
        .client
        .role(Role::Attestor)
        .context("blessing on a development cluster takes its attestor")?;
    served
        .platform
        .bless_service(
            &served.client,
            key,
            attestor,
            service_name(kind),
            &key.pubkey(),
        )
        .await
        .with_context(|| format!("blessing the {} on {}", service_name(kind), served.key))?;
    Ok(())
}

/// Whether a blessing that ends at `until` wants renewing at `now`: not
/// counted at all, or half of `ttl` or less left.
pub fn due(blessed: bool, until: u64, now: u64, ttl: u64) -> bool {
    !blessed || until < now.saturating_add(ttl / 2)
}

/// Keep the role key blessed on a development cluster, whose clock runs fast,
/// for as long as the service runs.
fn keep_blessed_development(
    kind: shonin_rootkey::ImageKind,
    served: SolanaServed,
    key: Arc<Keypair>,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let standing = served
                .platform
                .service_standing(&served.client, &key.pubkey(), kind.to_solana())
                .await;
            let wanted = match standing {
                Ok(standing) => due(standing.blessed, standing.until, standing.now, standing.ttl),
                Err(error) => {
                    tracing::debug!(
                        error = format!("{error:#}"),
                        "the Solana key's blessing could not be read"
                    );
                    false
                }
            };
            if wanted && let Err(error) = bless_development(kind, &served, &key).await {
                tracing::warn!(
                    error = format!("{error:#}"),
                    "the Solana key could not be blessed again; trying later"
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_is_taken_only_with_lamports_for_its_sends() {
        assert!(!stocked("devnet", 0));
        assert!(!stocked("devnet", WORK_LAMPORTS_PUBLIC - 1));
        assert!(stocked("devnet", WORK_LAMPORTS_PUBLIC));
        assert!(!stocked("localnet", WORK_LAMPORTS_PUBLIC));
        assert!(stocked("localnet", WORK_LAMPORTS_DEVELOPMENT));
    }

    #[test]
    fn a_development_blessing_is_renewed_at_half_its_length() {
        assert!(due(false, 1_000, 0, 100));
        assert!(!due(true, 150, 100, 100));
        assert!(due(true, 149, 100, 100));
    }
}
