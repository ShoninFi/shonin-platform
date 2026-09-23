//! Publishing a strategy.
//!
//! Creating the vault is publishing: the author funds its tank in the same
//! transaction, and the build and the review are the first things that tank
//! pays for. The build service will not admit the job until that vault is
//! there, so nothing is ever built for free.

use std::path::Path;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use anyhow::{Context, Result, bail};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::usd::ILocalUsd;
use chain::{Client, Deployments};
use shared::manifest::{Manifest, Prices, hash_bytes, hash_to_bytes};
use shared::model::registry::JobState;
use shared::{Submission, SubmissionAccepted, SubmissionSource};

/// What a publish produced.
pub struct Published {
    pub slug: String,
    pub vault: Address,
    pub job_id: String,
    pub source_hash: String,
    pub outcome: JobState,
}

/// Pack a project, create its vault, and submit it to be built.
pub async fn publish(
    chain: &Client,
    deployments: &Deployments,
    api: &str,
    project: &Path,
    author: Address,
    sealed: bool,
) -> Result<Published> {
    let manifest = read_manifest(project)?;
    let slug = manifest.slug();
    let archive = crate::publish::pack(project)?;
    let source_hash = hash_bytes(&archive);

    let vault = create_vault(chain, deployments, &manifest, author, &source_hash).await?;
    tracing::debug!(%vault, slug, "the vault is open for its build");

    let submission = Submission {
        strategy_id: manifest.strategy.id.clone(),
        version: manifest.strategy.version,
        author: format!("{author:?}"),
        source: SubmissionSource::Archive {
            base64: encode(&archive),
        },
        package: manifest.strategy.id.clone(),
        sealed,
    };
    let accepted = offer(api, &submission).await?;

    let outcome = follow(api, &accepted.job_id).await?;
    Ok(Published {
        slug,
        vault,
        job_id: accepted.job_id,
        source_hash,
        outcome,
    })
}

/// Create the vault, funding its tank in the same transaction.
async fn create_vault(
    chain: &Client,
    deployments: &Deployments,
    manifest: &Manifest,
    author: Address,
    source_hash: &str,
) -> Result<Address> {
    let config = chain
        .read(deployments.factory, IVaultFactory::configCall {})
        .await?;
    let tank = config.minimumTank;

    // What a day at the allowances this strategy declared costs, at the
    // published prices. The contract will not let a usage statement charge
    // more than this.
    let daily = Prices::published().daily_cap(&manifest.runtime.allowances);
    let daily_cap = U256::from((daily * 1_000_000.0).ceil() as u128);

    chain
        .send(
            author,
            deployments.usd,
            ILocalUsd::approveCall {
                spender: deployments.factory,
                value: tank,
            },
        )
        .await
        .context("the tank could not be approved")?;

    let strategy_id = name_to_bytes(&manifest.strategy.id);
    let hash = hash_to_bytes(source_hash).context("the source hash")?;

    // A version is published once. Saying so here is kinder than a reverted
    // transaction, and the answer is always the same: bump the version in the
    // manifest, because a new version is a new vault.
    let existing = chain
        .read(
            deployments.factory,
            IVaultFactory::vaultOfCall {
                strategyId: strategy_id.into(),
                version: manifest.strategy.version,
            },
        )
        .await
        .context("asking whether this version already has a vault")?;
    if existing != alloy::primitives::Address::ZERO {
        anyhow::bail!(
            "{} v{} already has a vault at {existing:?}. A version is published once: raise \
             `version` in the manifest and publish that.",
            manifest.strategy.id,
            manifest.strategy.version
        );
    }
    chain
        .send(
            author,
            deployments.factory,
            IVaultFactory::createCall {
                strategyId: strategy_id.into(),
                version: manifest.strategy.version,
                sourceHash: hash.into(),
                tankFunding: tank,
                dailyUsageCap: daily_cap,
            },
        )
        .await
        .context("the vault could not be created")?;

    let vault = chain
        .read(
            deployments.factory,
            IVaultFactory::vaultOfCall {
                strategyId: strategy_id.into(),
                version: manifest.strategy.version,
            },
        )
        .await?;
    Ok(vault)
}

/// Offer the submission, waiting for the build service to be ready.
///
/// A service that starts with the chain waits out its image's timelock before
/// it can sign anything, so arriving early is ordinary rather than an error.
async fn offer(api: &str, submission: &Submission) -> Result<SubmissionAccepted> {
    let client = reqwest::Client::new();
    let mut last = String::new();
    for attempt in 0..60 {
        let reply = client
            .post(format!("{api}/api/submissions"))
            .json(submission)
            .send()
            .await;
        match reply {
            Ok(reply) if reply.status().is_success() => {
                return reply
                    .json()
                    .await
                    .context("the build service said something unexpected");
            }
            Ok(reply) => {
                let status = reply.status();
                last = reply.text().await.unwrap_or_default();
                last = format!("{status}: {}", last.trim());
            }
            Err(error) => last = error.to_string(),
        }
        if attempt == 0 {
            tracing::info!("waiting for the build service");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("the build service never took the submission ({last})")
}

/// Watch a job until it finishes. The queue is public, so this is the same
/// view anyone else has of it.
async fn follow(api: &str, job_id: &str) -> Result<JobState> {
    let client = reqwest::Client::new();
    for _ in 0..600 {
        let job: shared::model::registry::Job = client
            .get(format!("{api}/api/jobs/{job_id}"))
            .send()
            .await
            .context("the queue did not answer")?
            .json()
            .await
            .context("the queue said something unexpected")?;
        match &job.state {
            JobState::Succeeded | JobState::Failed { .. } => return Ok(job.state),
            _ => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    bail!("the build is still running after ten minutes")
}

pub fn read_manifest(project: &Path) -> Result<Manifest> {
    let path = project.join("shonin.toml");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Manifest::from_toml(&text).context("the manifest is not valid")
}

/// Pack the project the way the build service unpacks it: the whole tree the
/// strategy lives in, so a workspace builds the same way here and there.
pub fn pack(project: &Path) -> Result<Vec<u8>> {
    let root = workspace_root(project);
    shared::archive::pack(&root).with_context(|| format!("packing {}", root.display()))
}

/// The directory a build has to start from: the workspace the strategy is a
/// member of, if there is one.
///
/// A strategy usually sits a few directories down from its workspace, with
/// nothing but folders in between, so the search keeps climbing past a
/// directory that holds no manifest at all.
fn workspace_root(project: &Path) -> std::path::PathBuf {
    let mut found = project.to_path_buf();
    let mut here = project.to_path_buf();
    for _ in 0..5 {
        let Some(parent) = here.parent().map(Path::to_path_buf) else {
            break;
        };
        let cargo = parent.join("Cargo.toml");
        if std::fs::read_to_string(&cargo)
            .map(|text| text.contains("[workspace]"))
            .unwrap_or(false)
        {
            found = parent.clone();
        }
        here = parent;
    }
    found
}

fn name_to_bytes(name: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let bytes = name.as_bytes();
    let take = bytes.len().min(32);
    out[..take].copy_from_slice(&bytes[..take]);
    out
}

fn encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut buffer = [0u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let value = u32::from_be_bytes([0, buffer[0], buffer[1], buffer[2]]);
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(ALPHABET[((value >> (18 - index * 6)) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_strategy_packs_the_workspace_it_belongs_to() {
        let root = std::env::temp_dir().join(format!("shonin-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("packages/agents/one");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(root.join("Cargo.toml"), b"[workspace]\nmembers = []\n").unwrap();
        std::fs::write(project.join("Cargo.toml"), b"[package]\n").unwrap();
        assert_eq!(workspace_root(&project), root);
    }

    #[test]
    fn a_strategy_with_no_workspace_packs_itself() {
        let root = std::env::temp_dir().join(format!("shonin-alone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Cargo.toml"), b"[package]\n").unwrap();
        assert_eq!(workspace_root(&root), root);
    }
}
