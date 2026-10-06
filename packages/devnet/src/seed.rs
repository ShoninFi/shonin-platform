//! Putting something on the platform, by publishing it.
//!
//! Nothing here is a fixture. Each agent is packed, submitted, built,
//! registered and reviewed exactly as an author's would be, so what the site
//! shows afterwards is what the platform did. That includes the unhappy ones:
//! a build that fails, a source nobody published, a vault the admin stops.

use std::path::{Path, PathBuf};

use alloy::primitives::U256;
use anyhow::{Context, Result, bail};
use chain::bindings::erc20::IERC20;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::IStation;
use chain::bindings::usd::ILocalUsd;
use chain::bindings::vault::{ClaimState, IVault, VaultState};
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
    /// Its author deposits too, then asks it all back while the agent has
    /// the money at the venue, so a claim is booked and served.
    claim: bool,
    /// Its author retires it once a depositor is in.
    retired: bool,
    /// Stop it once a depositor's money is at work, then see the vault close
    /// with its cash home and the depositor paid out.
    killed: bool,
}

const PLAN: &[Plan] = &[
    Plan {
        title: "Funding basis",
        sealed: false,
        broken: false,
        activate: true,
        deposit: true,
        claim: true,
        retired: false,
        killed: false,
    },
    Plan {
        title: "Funding basis, broken",
        sealed: false,
        broken: true,
        activate: false,
        deposit: false,
        claim: false,
        retired: false,
        killed: false,
    },
    Plan {
        title: "Funding basis, not yet activated",
        sealed: false,
        broken: false,
        activate: false,
        deposit: false,
        claim: false,
        retired: false,
        killed: false,
    },
    Plan {
        title: "Funding basis, retired",
        sealed: false,
        broken: false,
        activate: true,
        deposit: true,
        claim: false,
        retired: true,
        killed: false,
    },
    Plan {
        title: "Funding basis, stopped",
        sealed: false,
        broken: false,
        activate: true,
        deposit: true,
        claim: false,
        retired: false,
        killed: true,
    },
    // Last, so it is still new on the fast local clock when the seed ends.
    Plan {
        title: "Funding basis, sealed",
        sealed: true,
        broken: false,
        activate: true,
        deposit: false,
        claim: false,
        retired: false,
        killed: false,
    },
];

/// How long, in seconds, the seed waits for the keeper or the agent to take
/// each step: long enough for a real chain's blocks and a public endpoint's
/// pace, not only the local chains'.
const WAIT_SECONDS: u32 = 600;

/// What the seeded depositor puts into the live vault.
const DEPOSIT: u64 = 25_000_000_000;
/// What the author puts in and asks back, to book a claim.
const CLAIM_DEPOSIT: u64 = 20_000_000_000;

/// Publish one project as it stands: what an author does from the web app,
/// run from a checkout of their work. It is activated once it is ready.
///
/// With `live`, the vault is then used as the full seed's live vault is: a
/// depositor's request priced at a later valuation, and the author's own
/// deposit asked back once the agent has put it to work, booking a claim the
/// keeper serves. On a testnet that is the proof the vault runs on a real
/// chain, so each wait is long enough for a real chain's blocks.
pub async fn one(
    client: &Client,
    vault_chain: shared::Chain,
    deployments: &Deployments,
    api: &str,
    agents: Option<&Path>,
    name: &str,
    live: bool,
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
    // A manifest names one vault chain, and the build service refuses it on
    // any other: to publish the agent on another, a copy names this one.
    let moved = on_vault_chain(&agents, project, vault_chain)?;
    let project = moved.as_deref().unwrap_or(project);
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
            if live {
                let vault = published
                    .vault
                    .context("the strategy was published but no vault was activated")?;
                let slug = published.slug(vault_chain).unwrap_or_default();
                let viewer = client.account(Role::Viewer)?;
                deposit(client, deployments, vault, viewer, DEPOSIT).await?;
                tracing::info!(slug, "a depositor asked to put money in");
                let shares = wait_for_shares(client, vault, viewer).await?;
                tracing::info!(slug, %shares, "and the deposit was priced at a later valuation");
                claim(client, deployments, vault, author, &slug).await?;
            }
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
            deposit(client, deployments, vault, viewer, DEPOSIT).await?;
            tracing::info!(slug, "and a depositor asked to put money in");
        }
        if plan.claim {
            claim(client, deployments, vault, author, &slug).await?;
        }
        if plan.retired {
            wait_for_shares(client, vault, client.account(Role::Viewer)?).await?;
            client
                .send(author, vault, IVault::retireCall {})
                .await
                .context("the vault would not retire")?;
            tracing::info!(slug, "and then its author retired it");
        }
        if plan.killed {
            killed(client, deployments, vault, &slug).await?;
        }
    }

    let vaults = client
        .read(deployments.factory, IVaultFactory::vaultsCall {})
        .await?;
    tracing::info!(count = vaults.len(), "the platform has something on it");
    Ok(())
}

/// Ask to deposit, with the depositor's signed statement. The next valuation
/// prices it.
async fn deposit(
    client: &Client,
    deployments: &Deployments,
    vault: alloy::primitives::Address,
    who: alloy::primitives::Address,
    amount: u64,
) -> Result<()> {
    let amount = U256::from(amount);
    client
        .send(
            who,
            deployments.usd,
            ILocalUsd::approveCall {
                spender: vault,
                value: amount,
            },
        )
        .await?;
    let restricted = client.read(vault, IVault::restrictedTextCall {}).await?;
    let statement = client.sign_statement(who, vault, &restricted).await?;
    client
        .send(
            who,
            vault,
            IVault::requestDepositCall {
                assets: amount,
                receiver: who,
                statement,
            },
        )
        .await
        .context("the seeded deposit")?;
    Ok(())
}

/// Wait until a deposit has been priced and `who` holds shares.
async fn wait_for_shares(
    client: &Client,
    vault: alloy::primitives::Address,
    who: alloy::primitives::Address,
) -> Result<U256> {
    for _ in 0..WAIT_SECONDS {
        let held = client
            .read(vault, IVault::balanceOfCall { owner: who })
            .await?;
        if !held.is_zero() {
            return Ok(held);
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    bail!("a deposit was never priced: is the keeper running?")
}

/// How often, while the seed waits for the agent to put money to work, it
/// posts the stand-in venue's prices and funding again: each post is an
/// event the agent wakes to.
const NUDGE_EVERY_SECONDS: u32 = 20;

/// The funding rate the seed posts for the stand-in venue's perpetual while
/// it waits, as a fraction of the mark a day: five times what
/// `funding-basis` needs to start carrying (0.0002), and about what the local
/// market's own walk averages.
const NUDGED_FUNDING: (u64, u64) = (1, 1_000);

/// The author deposits, waits for the agent to put the money to work, then
/// asks for all of it back: more than the vault's cash, so a claim is booked
/// and the keeper serves it. Fails if no claim is booked.
///
/// Nothing moves the stand-in venue on a testnet, and `funding-basis` puts
/// money to work only when the venue's funding pays for it and the venue
/// tells it something happened. So while it waits the seed plays the market
/// operator for the run, as the local chain's market does every block:
/// it posts the venue's prices as they stand with a funding rate the agent
/// acts on. The withdrawal is asked for only once the station holds
/// positions at the venue and the vault's cash is under nine tenths of what
/// the author asks back; a position isn't money the agent can bring home
/// before the next valuation, so the cash can't cover it and a claim is
/// booked.
async fn claim(
    client: &Client,
    deployments: &Deployments,
    vault: alloy::primitives::Address,
    author: alloy::primitives::Address,
    slug: &str,
) -> Result<()> {
    use chain::bindings::venue::ILocalVenue;
    deposit(client, deployments, vault, author, CLAIM_DEPOSIT).await?;
    let shares = wait_for_shares(client, vault, author).await?;
    let station = client.read(vault, IVault::homeStationCall {}).await?;
    let mut at_work = false;
    for second in 0..WAIT_SECONDS {
        if second % NUDGE_EVERY_SECONDS == 0 {
            if let Err(error) = nudge_the_market(client, deployments).await {
                tracing::warn!(error = %format!("{error:#}"), "the stand-in venue's prices weren't posted");
            } else if second == 0 {
                tracing::info!(
                    slug,
                    "the seed posts the stand-in venue's prices and a funding rate the agent acts on, as its market operator"
                );
            }
        }
        let held = client
            .read(
                deployments.venue,
                ILocalVenue::marketsHeldByCall { master: station },
            )
            .await?;
        let cash = client.read(vault, IVault::cashCall {}).await?;
        if !held.is_empty()
            && cash * U256::from(10u64) < U256::from(CLAIM_DEPOSIT) * U256::from(9u64)
        {
            at_work = true;
            tracing::info!(slug, %cash, markets = held.len(), "the agent has put the money to work");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    if !at_work {
        bail!(
            "the agent never put the money to work within {WAIT_SECONDS} s, so a withdrawal would be paid from cash and no claim booked"
        );
    }
    let claims_before = client
        .read(vault, IVault::claimCountCall {})
        .await?
        .to::<u64>();
    client
        .send(
            author,
            vault,
            IVault::requestRedeemCall {
                shares,
                receiver: author,
            },
        )
        .await
        .context("the seeded withdrawal")?;
    let request = client.read(vault, IVault::requestCountCall {}).await?;
    let mut booked = None;
    for _ in 0..WAIT_SECONDS {
        let count = client
            .read(vault, IVault::claimCountCall {})
            .await?
            .to::<u64>();
        for id in claims_before + 1..=count {
            let claim = client
                .read(
                    vault,
                    IVault::claimAtCall {
                        claimId: U256::from(id),
                    },
                )
                .await?;
            if claim.owner == author {
                booked = Some(id);
            }
        }
        if booked.is_some() {
            break;
        }
        if client
            .read(vault, IVault::requestAtCall { requestId: request })
            .await?
            .done
        {
            bail!("the seeded withdrawal was paid from cash, so no claim was booked");
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let Some(claim_id) = booked else {
        bail!(
            "the seeded withdrawal booked no claim within {WAIT_SECONDS} s: is the keeper running?"
        );
    };
    tracing::info!(
        slug,
        claim = claim_id,
        "and a withdrawal larger than the cash booked a claim"
    );
    // The keeper serves it: the leaver's fraction is closed and they are
    // paid.
    for _ in 0..WAIT_SECONDS {
        let claim = client
            .read(
                vault,
                IVault::claimAtCall {
                    claimId: U256::from(claim_id),
                },
            )
            .await?;
        if matches!(claim.state, ClaimState::Paid) {
            tracing::info!(slug, paid = %claim.paid, "and the claim was paid");
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    bail!("a claim was booked but never paid: is the keeper running?")
}

/// Post the stand-in venue's prices as they stand, with a funding rate on its
/// perpetual of [`NUDGED_FUNDING`] of the mark a day, as its market operator.
async fn nudge_the_market(client: &Client, deployments: &Deployments) -> Result<()> {
    use chain::bindings::venue::{Funding, ILocalVenue, Quote};
    let operator = client.account(Role::Operator)?;
    let mut quotes = Vec::new();
    let mut funding = Vec::new();
    for name in [crate::deploy::SPOT, crate::deploy::PERP] {
        let id = crate::deploy::market_id(name);
        let market = client
            .read(deployments.venue, ILocalVenue::marketOfCall { market: id })
            .await?;
        quotes.push(Quote {
            market: id,
            mark: market.mark,
            oracle: market.oracle,
            bid: market.bid,
            ask: market.ask,
        });
        if market.isPerp {
            let rate = market.mark * U256::from(NUDGED_FUNDING.0) / U256::from(NUDGED_FUNDING.1);
            funding.push(Funding {
                market: id,
                ratePerDay: alloy::primitives::I256::try_from(rate)?,
                paidPerUnit: alloy::primitives::I256::ZERO,
            });
        }
    }
    client
        .send(
            operator,
            deployments.venue,
            ILocalVenue::postCall { quotes, funding },
        )
        .await
        .context("posting the stand-in venue's prices")?;
    Ok(())
}

/// The depositor's money goes to work; the admin stops the vault; the keeper
/// stops the station once the grace has passed, brings everything home and
/// closes the vault; the depositor is paid from the closed vault.
async fn killed(
    client: &Client,
    deployments: &Deployments,
    vault: alloy::primitives::Address,
    slug: &str,
) -> Result<()> {
    let viewer = client.account(Role::Viewer)?;
    let shares = wait_for_shares(client, vault, viewer).await?;
    for _ in 0..WAIT_SECONDS {
        let cash = client.read(vault, IVault::cashCall {}).await?;
        if cash < U256::from(DEPOSIT) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let admin = client.account(Role::Admin)?;
    client
        .send(admin, vault, IVault::killCall {})
        .await
        .context("the vault would not stop")?;
    tracing::info!(slug, "and then stopped with a depositor's money at work");

    let mut closed = false;
    for _ in 0..300 {
        if matches!(
            client.read(vault, IVault::stateCall {}).await?,
            VaultState::Closed
        ) {
            closed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    if !closed {
        bail!("a stopped vault never closed: is the keeper running?");
    }
    let station = client.read(vault, IVault::homeStationCall {}).await?;
    let left = client.read(station, IStation::valueCall {}).await?;
    let before = client
        .read(deployments.usd, IERC20::balanceOfCall { account: viewer })
        .await?;
    client
        .send(
            viewer,
            vault,
            IVault::redeemClosedCall {
                shares,
                receiver: viewer,
            },
        )
        .await
        .context("the depositor's withdrawal from the closed vault")?;
    let after = client
        .read(deployments.usd, IERC20::balanceOfCall { account: viewer })
        .await?;
    let paid = after - before;
    tracing::info!(slug, %paid, station_mark = %left.mark, "and closed with its cash home; the depositor was paid");
    // Paid in full, less the transaction fee and fees for the time it ran.
    if paid < U256::from(DEPOSIT) * U256::from(95u64) / U256::from(100u64) {
        bail!("the depositor of a closed vault was paid {paid} of {DEPOSIT}");
    }
    Ok(())
}

/// Copy the agent into a scratch tree, under the title a plan asks for.
/// A copy of the agents with `project`'s manifest naming `vault_chain` as its
/// vault's chain, when it names another; none when it already names it.
fn on_vault_chain(
    agents: &Path,
    project: &Path,
    vault_chain: shared::Chain,
) -> Result<Option<PathBuf>> {
    let manifest = std::fs::read_to_string(project.join("shonin.toml"))?;
    let wanted = format!("chain = \"{}\"", vault_chain.name());
    let mut in_vault = false;
    let mut changed = false;
    let rewritten: Vec<String> = manifest
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_vault = trimmed == "[vault]";
            }
            if in_vault && trimmed.starts_with("chain = ") && trimmed != wanted {
                changed = true;
                wanted.clone()
            } else {
                line.to_string()
            }
        })
        .collect();
    if !changed {
        return Ok(None);
    }
    let into = std::env::temp_dir().join(format!("shonin-seed-{}", vault_chain.name()));
    let _ = std::fs::remove_dir_all(&into);
    copy_tree(agents, &into)?;
    let relative = project.strip_prefix(agents).unwrap_or(project);
    let moved = into.join(relative);
    std::fs::write(moved.join("shonin.toml"), rewritten.join("\n") + "\n")?;
    tracing::info!(
        chain = vault_chain.name(),
        "the manifest names another vault chain: publishing a copy that names this one"
    );
    Ok(Some(moved))
}

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
