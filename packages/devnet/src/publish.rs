//! Publishing an agent, the way the web app does it: upload the project, pay
//! the $1 from the same wallet, follow the job, and, once it is reviewed and
//! built, activate it.
//!
//! Publishing and activating are separate on purpose. Nothing is spent but
//! the $1 until the author has read the review and chosen to open a vault.

use std::path::Path;
use std::time::Duration;

use alloy::primitives::{Address, B256, U256};
use anyhow::{Context, Result, bail};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::publishing::IPublishing;
use chain::bindings::reviews::IReviewRegistry;
use chain::bindings::usd::ILocalUsd;
use chain::{Client, Deployments};
use shared::model::registry::{Job, JobState};
use shared::{Allowances, Strategy, Upload, UploadAccepted};

/// What a publish produced.
pub struct Published {
    pub job_id: String,
    pub outcome: JobState,
    /// Assigned when the $1 was paid.
    pub strategy_id: Option<u64>,
    /// Set once it is activated.
    pub vault: Option<Address>,
}

impl Published {
    pub fn slug(&self, chain: shared::Chain) -> Option<String> {
        self.strategy_id.map(|id| Strategy::slug_for(chain, id))
    }
}

/// Upload a project, pay for it, and follow it until it is ready, refused or
/// failed. If it is ready and `activate` is set, activate it too.
#[allow(clippy::too_many_arguments)]
pub async fn publish(
    chain: &Client,
    vault_chain: shared::Chain,
    deployments: &Deployments,
    api: &str,
    project: &Path,
    author: Address,
    sealed: bool,
    activate: bool,
) -> Result<Published> {
    let archive = pack(project)?;
    let accepted = upload(
        api,
        vault_chain,
        &Upload {
            author: format!("{author:#x}"),
            archive_base64: shared::base64::encode(&archive),
            sealed,
            package: package_of(project),
        },
    )
    .await?;

    let reference: B256 = accepted.upload.parse().context("the upload's reference")?;
    pay(chain, deployments, author, reference).await?;

    let job = follow(api, &accepted.job_id).await?;
    let mut published = Published {
        job_id: accepted.job_id,
        outcome: job.state.clone(),
        strategy_id: job.strategy_id,
        vault: None,
    };
    if activate && job.state == JobState::Ready {
        let id = job.strategy_id.context("a ready job has an id")?;
        published.vault = Some(activate_it(chain, deployments, author, id).await?);
    }
    Ok(published)
}

/// Pay the $1 against the upload, from the wallet that uploaded it.
async fn pay(
    chain: &Client,
    deployments: &Deployments,
    author: Address,
    upload: B256,
) -> Result<()> {
    let fee = chain
        .read(deployments.publishing, IPublishing::feeCall {})
        .await?;
    chain
        .send(
            author,
            deployments.usd,
            ILocalUsd::approveCall {
                spender: deployments.publishing,
                value: fee,
            },
        )
        .await
        .context("the fee could not be approved")?;
    chain
        .send(
            author,
            deployments.publishing,
            IPublishing::submitCall { upload },
        )
        .await
        .context("the fee could not be paid")?;
    Ok(())
}

/// Wait for the review to be posted, then activate with the minimum tank and
/// the modest allowances.
pub async fn activate_it(
    chain: &Client,
    deployments: &Deployments,
    author: Address,
    id: u64,
) -> Result<Address> {
    let strategy_id = U256::from(id);
    for attempt in 0.. {
        let reviewed = chain
            .read(
                deployments.reviews,
                IReviewRegistry::hasReviewCall {
                    strategyId: strategy_id,
                },
            )
            .await?;
        if reviewed {
            break;
        }
        if attempt == 120 {
            bail!("the review of {id} was never posted");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let tank = chain
        .read(deployments.factory, IVaultFactory::configCall {})
        .await?
        .minimumTank;
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
    // The author signs the depositor's statement for the vault before it
    // exists, as every holder does, and names the terms they read.
    let vault = chain
        .read(
            deployments.factory,
            IVaultFactory::predictVaultCall {
                strategyId: strategy_id,
            },
        )
        .await?;
    let restricted = chain
        .read(
            deployments.factory,
            IVaultFactory::restrictedForCall {
                strategyId: strategy_id,
            },
        )
        .await?
        .text;
    let statement = chain.sign_statement(author, vault, &restricted).await?;
    let terms = chain
        .read(deployments.factory, IVaultFactory::termsCall {})
        .await?;
    chain
        .send(
            author,
            deployments.factory,
            IVaultFactory::activateCall {
                strategyId: strategy_id,
                allowances: chain::allowances(&Allowances::modest()),
                idleFloorBps: 500,
                tankFunding: tank,
                tankKeep: alloy::primitives::U256::ZERO,
                statement,
                terms,
            },
        )
        .await
        .context("the vault could not be activated")?;
    chain
        .read(
            deployments.factory,
            IVaultFactory::vaultOfCall {
                strategyId: strategy_id,
            },
        )
        .await
        .context("reading the new vault")
}

/// Offer the project, waiting for the build service to be ready.
///
/// A service that starts with the chain waits out its image's timelock before
/// it can sign anything, so arriving early is ordinary rather than an error.
async fn upload(api: &str, chain: shared::Chain, upload: &Upload) -> Result<UploadAccepted> {
    let client = reqwest::Client::new();
    let mut last = String::new();
    for attempt in 0..60 {
        let reply = client
            .post(format!("{api}/api/{chain}/uploads"))
            .json(upload)
            .send()
            .await;
        match reply {
            Ok(reply) if reply.status().is_success() => {
                return reply
                    .json()
                    .await
                    .context("the build service said something unexpected");
            }
            // A refusal of the project itself won't change by waiting.
            Ok(reply) if reply.status() == reqwest::StatusCode::BAD_REQUEST => {
                bail!(
                    "the build service refused the project: {}",
                    reply.text().await.unwrap_or_default()
                );
            }
            Ok(reply) => {
                let status = reply.status();
                last = format!(
                    "{status}: {}",
                    reply.text().await.unwrap_or_default().trim()
                );
            }
            Err(error) => last = error.to_string(),
        }
        if attempt == 0 {
            tracing::info!("waiting for the build service");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("the build service never took the upload ({last})")
}

/// Watch a job until it finishes. The queue is public, so this is the same
/// view anyone else has of it.
async fn follow(api: &str, job_id: &str) -> Result<Job> {
    let client = reqwest::Client::new();
    // A review by a model takes minutes; a build, a minute or two more.
    for _ in 0..1_800 {
        let job: Job = client
            .get(format!("{api}/api/jobs/{job_id}"))
            .send()
            .await
            .context("the queue did not answer")?
            .json()
            .await
            .context("the queue said something unexpected")?;
        if job.state.is_finished() {
            return Ok(job);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    bail!("job {job_id} is still running after half an hour")
}

/// The cargo package a project is, from its `Cargo.toml`.
fn package_of(project: &Path) -> Option<String> {
    let text = std::fs::read_to_string(project.join("Cargo.toml")).ok()?;
    text.lines().find_map(|line| {
        line.strip_prefix("name = ")
            .map(|name| name.trim().trim_matches('"').to_string())
    })
}

/// Pack the project the way the build service unpacks it: the whole tree the
/// agent lives in, so a workspace builds the same way here and there.
pub fn pack(project: &Path) -> Result<Vec<u8>> {
    let root = workspace_root(project);
    shared::archive::pack(&root).with_context(|| format!("packing {}", root.display()))
}

/// The directory a build has to start from: the workspace the agent is a
/// member of, if there is one.
///
/// An agent usually sits a few directories down from its workspace, with
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_packs_the_workspace_it_belongs_to() {
        let root = std::env::temp_dir().join(format!("shonin-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("packages/agents/one");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(root.join("Cargo.toml"), b"[workspace]\nmembers = []\n").unwrap();
        std::fs::write(project.join("Cargo.toml"), b"[package]\nname = \"one\"\n").unwrap();
        assert_eq!(workspace_root(&project), root);
        assert_eq!(package_of(&project).as_deref(), Some("one"));
    }

    #[test]
    fn an_agent_with_no_workspace_packs_itself() {
        let root = std::env::temp_dir().join(format!("shonin-alone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Cargo.toml"), b"[package]\n").unwrap();
        assert_eq!(workspace_root(&root), root);
    }
}
