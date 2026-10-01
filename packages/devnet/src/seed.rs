//! Putting something on the platform, by publishing it.
//!
//! Nothing here is a fixture. Each agent is packed, submitted, built,
//! registered and reviewed exactly as an author's would be, so what the site
//! shows afterwards is what the platform did. That includes the unhappy ones:
//! a build that fails, a source nobody published, a vault the admin stops.

use std::path::{Path, PathBuf};

use alloy::primitives::U256;
use anyhow::{Context, Result, bail};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::usd::ILocalUsd;
use chain::bindings::vault::IVault;
use chain::{Client, Deployments, Role};
use shared::model::registry::JobState;

/// What the site needs on it before every page has something to show.
struct Plan {
    /// The title it is published under, since a changed agent is a new agent.
    title: &'static str,
    /// Keep the source out of the store.
    sealed: bool,
    /// Break it on purpose, so a failed build is on the queue.
    broken: bool,
    /// Activate it once it is ready.
    activate: bool,
    /// A depositor puts money in once it is open.
    deposit: bool,
    /// Stop it once it is running.
    killed: bool,
}

const PLAN: &[Plan] = &[
    Plan {
        title: "Funding basis",
        sealed: false,
        broken: false,
        activate: true,
        deposit: true,
        killed: false,
    },
    Plan {
        title: "Funding basis, sealed",
        sealed: true,
        broken: false,
        activate: true,
        deposit: false,
        killed: false,
    },
    Plan {
        title: "Funding basis, broken",
        sealed: false,
        broken: true,
        activate: false,
        deposit: false,
        killed: false,
    },
    Plan {
        title: "Funding basis, not yet activated",
        sealed: false,
        broken: false,
        activate: false,
        deposit: false,
        killed: false,
    },
    Plan {
        title: "Funding basis, stopped",
        sealed: false,
        broken: false,
        activate: true,
        deposit: false,
        killed: true,
    },
];

/// What the seeded depositor puts into the live vault.
const DEPOSIT: u64 = 25_000_000_000;

/// Publish one project as it stands: what an author does from the web app,
/// run from a checkout of their work. It is activated once it is ready.
pub async fn one(
    client: &Client,
    vault_chain: shared::Chain,
    deployments: &Deployments,
    api: &str,
    agents: Option<&Path>,
    name: &str,
) -> Result<()> {
    let agents = agents
        .map(Path::to_path_buf)
        .or_else(guess_agents)
        .context("where the agents live: pass --agents or set SHONIN_AGENTS_DIR")?;
    let project = agents.join("packages/agents").join(name);
    let project = project.as_path();
    if !project.join("shonin.toml").exists() {
        bail!("no agent at {}", project.display());
    }
    let author = client.account(Role::Author)?;
    say_who_the_author_is(client, deployments, author).await?;
    tracing::info!(project = %project.display(), "publishing");
    let published = crate::publish::publish(
        client,
        vault_chain,
        deployments,
        api,
        project,
        author,
        false,
        true,
    )
    .await?;
    match published.outcome {
        JobState::Ready => {
            tracing::info!(slug = published.slug(vault_chain), vault = ?published.vault, job = published.job_id, "published and activated");
            Ok(())
        }
        other => bail!("job {} ended {}", published.job_id, other.label()),
    }
}

pub async fn run(
    client: &Client,
    vault_chain: shared::Chain,
    deployments: &Deployments,
    api: &str,
    agents: Option<&Path>,
) -> Result<()> {
    let agents = agents
        .map(Path::to_path_buf)
        .or_else(guess_agents)
        .context("where the agents live: pass --agents or set SHONIN_AGENTS_DIR")?;
    let agent = agents.join("packages/agents/funding-basis");
    if !agent.join("shonin.toml").exists() {
        bail!("no agent at {}", agent.display());
    }

    let author = client.account(Role::Author)?;
    say_who_the_author_is(client, deployments, author).await?;

    let work = std::env::temp_dir().join("shonin-seed");
    let _ = std::fs::remove_dir_all(&work);

    for (index, plan) in PLAN.iter().enumerate() {
        let prepared = prepare(&agents, &agent, &work.join(index.to_string()), plan)?;
        tracing::info!(title = plan.title, "publishing");
        let published = crate::publish::publish(
            client,
            vault_chain,
            deployments,
            api,
            &prepared,
            author,
            plan.sealed,
            plan.activate,
        )
        .await?;
        let slug = published.slug(vault_chain).unwrap_or_default();
        match (&published.outcome, plan.broken) {
            (JobState::Failed { outcome }, true) => {
                tracing::info!(
                    slug,
                    outcome = outcome.label(),
                    "the build failed, as intended"
                )
            }
            (JobState::Ready, false) => tracing::info!(slug, "published"),
            (other, _) => bail!("{} ended {}, not as planned", plan.title, other.label()),
        }

        let Some(vault) = published.vault else {
            continue;
        };
        if plan.deposit {
            let viewer = client.account(Role::Viewer)?;
            let amount = U256::from(DEPOSIT);
            client
                .send(
                    viewer,
                    deployments.usd,
                    ILocalUsd::approveCall {
                        spender: vault,
                        value: amount,
                    },
                )
                .await?;
            // A deposit is a request: the keeper's next valuation prices it.
            let restricted = client.read(vault, IVault::restrictedTextCall {}).await?;
            let statement = client.sign_typed(
                viewer,
                chain::statement::digest(client.chain_id, vault, viewer, &restricted),
            )?;
            client
                .send(
                    viewer,
                    vault,
                    IVault::requestDepositCall {
                        assets: amount,
                        receiver: viewer,
                        statement,
                    },
                )
                .await
                .context("the seeded deposit")?;
            tracing::info!(slug, "and a depositor asked to put money in");
        }
        if plan.killed {
            let admin = client.account(Role::Admin)?;
            client
                .send(admin, vault, IVault::killCall {})
                .await
                .context("the vault would not stop")?;
            tracing::info!(slug, "and then stopped, so the feed has a kill");
        }
    }

    let vaults = client
        .read(deployments.factory, IVaultFactory::vaultsCall {})
        .await?;
    tracing::info!(count = vaults.len(), "the platform has something on it");
    Ok(())
}

/// Copy the agent into a scratch tree, under the title a plan asks for.
fn prepare(agents: &Path, agent: &Path, into: &Path, plan: &Plan) -> Result<PathBuf> {
    copy_tree(agents, into)?;
    let relative = agent
        .strip_prefix(agents)
        .unwrap_or(Path::new("packages/agents/funding-basis"));
    let project = into.join(relative);

    let manifest_path = project.join("shonin.toml");
    let manifest: String = std::fs::read_to_string(&manifest_path)?
        .lines()
        .map(|line| {
            if line.starts_with("title = ") {
                format!("title = \"{}\"", plan.title)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&manifest_path, manifest + "\n")?;

    if plan.broken {
        // A line that will not compile, so the queue has a failure on it.
        let source = project.join("src/lib.rs");
        let mut text = std::fs::read_to_string(&source)?;
        text.push_str("\nfn this_does_not_compile() -> u8 { \"not a number\" }\n");
        std::fs::write(&source, text)?;
    }
    Ok(project)
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    const SKIP: &[&str] = &["target", ".git", "node_modules", ".shonin"];
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if SKIP.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        let source = entry.path();
        let target = to.join(&name);
        if source.is_dir() {
            copy_tree(&source, &target)?;
        } else {
            std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

async fn say_who_the_author_is(
    client: &Client,
    deployments: &Deployments,
    author: alloy::primitives::Address,
) -> Result<()> {
    use chain::bindings::profiles::IAuthorProfiles;
    let existing = client
        .read(
            deployments.profiles,
            IAuthorProfiles::profileOfCall { author },
        )
        .await?;
    if !existing.handle.is_empty() {
        return Ok(());
    }
    client
        .send(
            author,
            deployments.profiles,
            IAuthorProfiles::setProfileCall {
                handle: "shonin".to_string(),
                blurb: "The platform's own reference agents. They are published like \
                        anyone else's, and their records are as public as anyone else's."
                    .to_string(),
                linkLabels: Vec::new(),
                linkUrls: Vec::new(),
            },
        )
        .await?;
    Ok(())
}

/// A sibling checkout, which is where they usually are.
fn guess_agents() -> Option<PathBuf> {
    let here = std::env::current_dir().ok()?;
    let parent = here.parent()?;
    for name in ["SHONIN-AGENTS", "shonin-agents", "agents"] {
        let candidate = parent.join(name);
        if candidate.join("Cargo.toml").exists() {
            return Some(candidate);
        }
    }
    None
}
