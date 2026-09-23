//! Putting something on the platform, by publishing it.
//!
//! Nothing here is a fixture. Each strategy is packed, submitted, built,
//! registered and reviewed exactly as an author's would be, so what the site
//! shows afterwards is what the platform did. That includes the unhappy ones:
//! a build that fails, a source nobody published, a vault the admin stops.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::vault::IVault;
use chain::{Client, Deployments, Role};
use shared::model::registry::JobState;

/// What the site needs on it before every page has something to show.
struct Plan {
    /// What to call it, and which version.
    id: &'static str,
    version: u32,
    /// The version it claims to replace.
    supersedes: Option<&'static str>,
    /// Keep the source out of the store.
    sealed: bool,
    /// Break it on purpose, so a failed build is on the queue where anyone
    /// can read why.
    broken: bool,
    /// Stop it once it is running.
    killed: bool,
}

const PLAN: &[Plan] = &[
    Plan {
        id: "funding-basis",
        version: 1,
        supersedes: None,
        sealed: false,
        broken: false,
        killed: false,
    },
    Plan {
        id: "funding-basis",
        version: 2,
        supersedes: Some("funding-basis@1"),
        sealed: false,
        broken: false,
        killed: false,
    },
    Plan {
        id: "funding-basis",
        version: 3,
        supersedes: None,
        sealed: true,
        broken: false,
        killed: false,
    },
    Plan {
        id: "funding-basis",
        version: 4,
        supersedes: None,
        sealed: false,
        broken: true,
        killed: false,
    },
    Plan {
        id: "funding-basis",
        version: 5,
        supersedes: None,
        sealed: false,
        broken: false,
        killed: true,
    },
];

pub async fn run(
    client: &Client,
    deployments: &Deployments,
    api: &str,
    bots: Option<&Path>,
    store: &Path,
) -> Result<()> {
    let bots = bots
        .map(Path::to_path_buf)
        .or_else(guess_bots)
        .context("where the strategies live: pass --bots or set SHONIN_BOTS_DIR")?;
    let strategy = bots.join("packages/bots/funding-basis");
    if !strategy.join("shonin.toml").exists() {
        bail!("no strategy at {}", strategy.display());
    }

    let author = client.account(Role::Author)?;
    say_who_the_author_is(client, deployments, author).await?;

    let work = std::env::temp_dir().join("shonin-seed");
    let _ = std::fs::remove_dir_all(&work);

    for plan in PLAN {
        let prepared = prepare(&bots, &strategy, &work, plan)?;
        tracing::info!(id = plan.id, version = plan.version, "publishing");

        let published =
            shonin_cli::publish(client, deployments, api, &prepared, author, plan.sealed).await?;

        match &published.outcome {
            JobState::Succeeded => tracing::info!(slug = published.slug, "published"),
            JobState::Failed { reason } => {
                tracing::info!(
                    slug = published.slug,
                    reason,
                    "the build failed, as intended"
                )
            }
            other => tracing::warn!(state = other.label(), "the build did not finish"),
        }

        if plan.killed && matches!(published.outcome, JobState::Succeeded) {
            let admin = client.account(Role::Admin)?;
            client
                .send(admin, published.vault, IVault::killCall {})
                .await
                .context("the vault would not stop")?;
            tracing::info!(
                slug = published.slug,
                "and then stopped, so the feed has a kill"
            );
        }
    }

    let _ = store;
    let vaults = client
        .read(deployments.factory, IVaultFactory::vaultsCall {})
        .await?;
    tracing::info!(count = vaults.len(), "the platform has something on it");
    Ok(())
}

/// Copy the strategy into a scratch tree, with the manifest a plan asks for.
fn prepare(bots: &Path, strategy: &Path, work: &Path, plan: &Plan) -> Result<PathBuf> {
    let into = work.join(format!("{}-v{}", plan.id, plan.version));
    copy_tree(bots, &into)?;

    let relative = strategy
        .strip_prefix(bots)
        .unwrap_or(Path::new("packages/bots/funding-basis"));
    let project = into.join(relative);

    let manifest_path = project.join("shonin.toml");
    let mut manifest = std::fs::read_to_string(&manifest_path)?;
    manifest = manifest.replace("version = 1", &format!("version = {}", plan.version));
    if let Some(claim) = plan.supersedes {
        manifest = manifest.replace(
            &format!("version = {}", plan.version),
            &format!("version = {}\nsupersedes = \"{claim}\"", plan.version),
        );
    }
    std::fs::write(&manifest_path, manifest)?;

    if plan.broken {
        // A line that will not compile, so the queue has a failure on it with
        // the compiler's own words underneath.
        let source = project.join("src/lib.rs");
        let mut text = std::fs::read_to_string(&source)?;
        text.push_str("\nfn this_does_not_compile() -> Decimal { \"not a decimal\" }\n");
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
                blurb: "The platform's own reference strategies. They are published like \
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
fn guess_bots() -> Option<PathBuf> {
    let here = std::env::current_dir().ok()?;
    let parent = here.parent()?;
    for name in ["SHONIN-BOTS", "shonin-bots", "bots"] {
        let candidate = parent.join(name);
        if candidate.join("Cargo.toml").exists() {
            return Some(candidate);
        }
    }
    None
}
