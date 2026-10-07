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
    /// The depositor asks it all back while the agent has part of the money
    /// at the venue, so a claim is booked and served; the author then puts
    /// a little in, so the vault keeps a depositor.
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

/// How long, in seconds of the clock, the seed waits for the keeper or the
/// agent to take each step: long enough for a real chain's blocks and a
/// public endpoint's pace, not only the local chains'.
const WAIT_SECONDS: u64 = 600;

/// How long, in seconds of the clock, the executor has to take a new vault
/// on (its key blessed, stocked with gas by the keeper, and approved at the
/// station) before the seed stops and says so, rather than waiting on an
/// agent that never starts: the default of `--take-on-seconds`. Where a
/// proof of the attestation is in the path, allow far longer.
pub const TAKE_ON_SECONDS: u64 = 180;

/// When a wait of `seconds` begun now ends, by the clock: a step's reads take
/// their own time on a slow or rate-limited endpoint, so turns of a loop are
/// no measure.
fn deadline(seconds: u64) -> tokio::time::Instant {
    tokio::time::Instant::now() + std::time::Duration::from_secs(seconds)
}

/// What the seeded depositor puts into the live vault.
const DEPOSIT: u64 = 25_000_000_000;
/// What the author puts into the live vault once the depositor's claim is
/// paid, so it keeps a depositor.
const STAYING_DEPOSIT: u64 = 5_000_000_000;

/// Publish one project as it stands: what an author does from the web app,
/// run from a checkout of their work. It is activated once it is ready.
///
/// With `live`, the vault is then used as the full seed's live vault is: a
/// depositor's request priced at a later valuation, and all of it asked back
/// once the agent has put part of it to work, booking a claim the keeper
/// serves; then a little of the author's, so it keeps a depositor. On a
/// testnet that is the proof the vault runs on a real chain, so each wait is
/// long enough for a real chain's blocks.
///
/// With `publish_only`, it is published and nothing more: no vault is
/// activated, and [`on_strategy`] runs the live part later (on a testnet
/// whose development checker is retired in between, say).
#[allow(clippy::too_many_arguments)]
pub async fn one(
    client: &Client,
    vault_chain: shared::Chain,
    deployments: &Deployments,
    api: &str,
    agents: Option<&Path>,
    name: &str,
    live: bool,
    publish_only: bool,
    take_on: u64,
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
        !publish_only,
    )
    .await?;
    match published.outcome {
        JobState::Ready if publish_only => {
            let id = published.strategy_id.context("a ready job has an id")?;
            tracing::info!(
                slug = published.slug(vault_chain),
                job = published.job_id,
                "published; no vault activated"
            );
            // For the task that runs the live part later.
            println!("strategy {id}");
            Ok(())
        }
        JobState::Ready => {
            tracing::info!(slug = published.slug(vault_chain), vault = ?published.vault, job = published.job_id, "published and activated");
            if live {
                let vault = published
                    .vault
                    .context("the strategy was published but no vault was activated")?;
                let slug = published.slug(vault_chain).unwrap_or_default();
                used_live(client, deployments, vault, &slug, take_on).await?;
            }
            Ok(())
        }
        other => bail!("job {} ended {}", published.job_id, other.label()),
    }
}

/// The live part of [`one`] on a strategy already published: its vault
/// activated if it has none yet, then a depositor in, a claim booked and
/// paid, and a little of the author's left in. Nothing is published, so no
/// build or review service is needed, and no checker but the one that
/// blesses the executor.
pub async fn on_strategy(
    client: &Client,
    vault_chain: shared::Chain,
    deployments: &Deployments,
    strategy: u64,
    take_on: u64,
) -> Result<()> {
    use chain::bindings::strategies::IStrategyRegistry;
    let id = U256::from(strategy);
    let registered = client
        .read(
            deployments.strategies,
            IStrategyRegistry::isRegisteredCall { strategyId: id },
        )
        .await?;
    if !registered {
        bail!("strategy {strategy} is not registered on this chain");
    }
    let slug = shared::Strategy::slug_for(vault_chain, strategy);
    let mut vault = client
        .read(
            deployments.factory,
            IVaultFactory::vaultOfCall { strategyId: id },
        )
        .await?;
    if vault.is_zero() {
        let author = client.account(Role::Author)?;
        vault = crate::publish::activate_it(client, deployments, author, strategy).await?;
        tracing::info!(slug, %vault, "activated");
    } else {
        tracing::info!(slug, %vault, "its vault is already activated");
    }
    used_live(client, deployments, vault, &slug, take_on).await
}

/// A depositor in, then the claim flow ([`claim`]).
async fn used_live(
    client: &Client,
    deployments: &Deployments,
    vault: alloy::primitives::Address,
    slug: &str,
    take_on: u64,
) -> Result<()> {
    let viewer = client.account(Role::Viewer)?;
    deposit(client, deployments, vault, viewer, DEPOSIT).await?;
    tracing::info!(slug, "a depositor asked to put money in");
    claim(client, deployments, vault, slug, take_on).await
}

pub async fn run(
    client: &Client,
    vault_chain: shared::Chain,
    deployments: &Deployments,
    api: &str,
    agents: Option<&Path>,
    take_on: u64,
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
            claim(client, deployments, vault, &slug, take_on).await?;
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
            killed(client, deployments, vault, &slug, take_on).await?;
        }
    }

    let vaults = client
        .read(deployments.factory, IVaultFactory::vaultsCall {})
        .await?;
    tracing::info!(count = vaults.len(), "the platform has something on it");
    Ok(())
}

/// What the demo asks of a run.
pub struct Demo<'a> {
    pub api: &'a str,
    pub agents: Option<&'a Path>,
    pub agent: &'a str,
    /// A strategy already published (with `--publish-only`) and never
    /// activated, to activate instead of publishing again.
    pub strategy: Option<u64>,
    /// The depositor's money, in dollars.
    pub deposit_dollars: u64,
    /// The tank is filled, and kept by auto-fill, at this many days of the
    /// allowances' daily cap.
    pub tank_days: u64,
    pub take_on: u64,
    /// How long the agent has to put the money to work once it is priced.
    pub work_within: std::time::Duration,
}

/// A demonstration that pays its own way: a fresh vault for one agent,
/// activated at the modest allowances with its tank on auto-fill, a
/// depositor's money in and at work, and then left running for nobody to top
/// up. The market is the operator's (`market`), not the seed's: nothing here
/// posts to the venue.
///
/// A vault belongs to one strategy for good, so a fresh vault is a fresh
/// publish (or a strategy published with `--publish-only` and never
/// activated). The tank starts at, and auto-fill keeps it at, `tank_days`
/// of the allowances' daily cap, gas allowance included; only the
/// depositor puts money in, so the author's only shares are fee shares, and
/// every top-up the tank gets after activation is paid from them.
pub async fn demo(
    client: &Client,
    vault_chain: shared::Chain,
    deployments: &Deployments,
    demo: &Demo<'_>,
) -> Result<()> {
    use chain::bindings::prices::IPrices;
    use chain::bindings::strategies::IStrategyRegistry;

    let author = client.account(Role::Author)?;
    let strategy = match demo.strategy {
        Some(id) => {
            let strategy_id = U256::from(id);
            let registered = client
                .read(
                    deployments.strategies,
                    IStrategyRegistry::isRegisteredCall {
                        strategyId: strategy_id,
                    },
                )
                .await?;
            if !registered {
                bail!("strategy {id} is not registered on this chain");
            }
            let vault = client
                .read(
                    deployments.factory,
                    IVaultFactory::vaultOfCall {
                        strategyId: strategy_id,
                    },
                )
                .await?;
            if !vault.is_zero() {
                bail!(
                    "strategy {id} already has its vault ({vault}): a demo runs on a fresh one, so publish again (--agent) or pass a strategy published with --publish-only"
                );
            }
            id
        }
        None => {
            let agents = demo
                .agents
                .map(Path::to_path_buf)
                .or_else(guess_agents)
                .context("where the agents live: pass --agents or set SHONIN_AGENTS_DIR")?;
            let project = agents.join("packages/agents").join(demo.agent);
            if !project.join("shonin.toml").exists() {
                bail!("no agent at {}", project.display());
            }
            let moved = on_vault_chain(&agents, &project, vault_chain)?;
            let project = moved.as_deref().unwrap_or(&project);
            say_who_the_author_is(client, deployments, author).await?;
            tracing::info!(project = %project.display(), "publishing, for a fresh vault");
            let published = crate::publish::publish(
                client,
                vault_chain,
                deployments,
                demo.api,
                project,
                author,
                false,
                false,
            )
            .await?;
            match published.outcome {
                JobState::Ready => published.strategy_id.context("a ready job has an id")?,
                other => bail!("job {} ended {}", published.job_id, other.label()),
            }
        }
    };
    let slug = shared::Strategy::slug_for(vault_chain, strategy);

    // Activated as an author would, at the modest allowances with the
    // smallest tank; then the tank filled to its figure and auto-fill set.
    let vault = crate::publish::activate_it(client, deployments, author, strategy).await?;
    tracing::info!(slug, %vault, "activated at the modest allowances");
    let allowances = client.read(vault, IVault::allowancesCall {}).await?;
    let daily = client
        .read(deployments.prices, IPrices::dailyCostCall { allowances })
        .await?;
    let keep = daily * U256::from(demo.tank_days);
    let tank = client.read(vault, IVault::tankCall {}).await?;
    let keep = keep.max(tank.balance);
    let more = keep - tank.balance;
    if !more.is_zero() {
        have(client, deployments, author, more).await?;
        client
            .send(
                author,
                deployments.usd,
                ILocalUsd::approveCall {
                    spender: vault,
                    value: more,
                },
            )
            .await?;
        client
            .send(author, vault, IVault::fundTankCall { assets: more })
            .await
            .context("filling the tank")?;
    }
    client
        .send(author, vault, IVault::setTankKeepCall { assets: keep })
        .await
        .context("turning auto-fill on")?;
    tracing::info!(
        slug,
        tank = %keep,
        daily_cap = %daily,
        days = demo.tank_days,
        "the tank is filled, and auto-fill keeps it there from the author's fee shares"
    );

    let viewer = client.account(Role::Viewer)?;
    let amount = demo.deposit_dollars * 1_000_000;
    have(client, deployments, viewer, U256::from(amount)).await?;
    deposit(client, deployments, vault, viewer, amount).await?;
    tracing::info!(
        slug,
        dollars = demo.deposit_dollars,
        "a depositor asked to put money in"
    );

    wait_taken_on(client, deployments, vault, &slug, demo.take_on).await?;
    let shares = wait_for_shares(client, vault, viewer).await?;
    tracing::info!(slug, %shares, "the deposit was priced at a later valuation");
    at_work(client, deployments, vault, &slug, shares, demo.work_within).await?;

    println!("vault {vault}");
    println!(
        "The demo is running and is left to run: {slug} ({vault}) on {}, ${} deposited, the tank at {} (auto-fill on).",
        vault_chain.name(),
        demo.deposit_dollars,
        alloy::primitives::utils::format_units(keep, 6).unwrap_or_default()
    );
    println!(
        "Keep the market operator posting (task chains:market NETWORK=testnet), and the services up. To watch it pay its way:"
    );
    println!(
        "  task services:economics NETWORK=testnet VAULT_CHAIN={} VAULT={vault}",
        vault_chain.name()
    );
    println!(
        "The bill is collected and auto-fill runs on the keeper's daily keep; the author's fee shares are minted at the first valuation on or after Monday 00:00 UTC."
    );
    Ok(())
}

/// Make sure `who` holds `amount` of the development dollar, minting what it
/// lacks (anyone may mint it).
async fn have(
    client: &Client,
    deployments: &Deployments,
    who: alloy::primitives::Address,
    amount: U256,
) -> Result<()> {
    let held = client
        .read(deployments.usd, IERC20::balanceOfCall { account: who })
        .await?;
    if held < amount {
        client
            .send(
                who,
                deployments.usd,
                ILocalUsd::mintCall {
                    to: who,
                    amount: amount - held,
                },
            )
            .await
            .context("minting the development dollar")?;
    }
    Ok(())
}

/// Wait until the agent holds positions at the venue and the vault's cash is
/// under nine tenths of what the depositor's shares are worth. The market is
/// left to its operator; if the agent hasn't moved within `within`, say what
/// the venue's funding is, since `funding-basis` waits for 0.0002 a day.
async fn at_work(
    client: &Client,
    deployments: &Deployments,
    vault: alloy::primitives::Address,
    slug: &str,
    shares: U256,
    within: std::time::Duration,
) -> Result<()> {
    use chain::bindings::venue::ILocalVenue;
    let station = client.read(vault, IVault::homeStationCall {}).await?;
    let until = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < until {
        let held = client
            .read(
                deployments.venue,
                ILocalVenue::marketsHeldByCall { master: station },
            )
            .await?;
        let cash = client.read(vault, IVault::cashCall {}).await?;
        let price = client.read(vault, IVault::sharePriceCall {}).await?;
        let worth = shares * price / U256::from(10u64).pow(U256::from(18u64));
        if !held.is_empty() && cash * U256::from(10u64) < worth * U256::from(9u64) {
            tracing::info!(slug, %cash, %worth, markets = held.len(), "the agent has put the money to work");
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
    let perp = client
        .read(
            deployments.venue,
            ILocalVenue::marketOfCall {
                market: crate::deploy::market_id(crate::deploy::PERP),
            },
        )
        .await?;
    let rate: f64 = perp.fundingRate.to_string().parse().unwrap_or(0.0);
    let mark: f64 = perp.mark.to_string().parse().unwrap_or(1.0);
    bail!(
        "the agent of {slug} ({vault}) has not put the money to work within {} min. The venue quotes funding of {:.6} a day, last posted at {}: funding-basis enters at 0.0002. Is the market operator posting (task chains:market NETWORK=testnet)? The vault is left as it is.",
        within.as_secs() / 60,
        rate / mark,
        perp.fundingUpdatedAt
    )
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
    let until = deadline(WAIT_SECONDS);
    while tokio::time::Instant::now() < until {
        let held = client
            .read(vault, IVault::balanceOfCall { owner: who })
            .await?;
        if !held.is_zero() {
            return Ok(held);
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    bail!("a deposit was never priced within {WAIT_SECONDS} s: is the keeper running?")
}

/// Wait, by the clock and for at most `take_on` seconds, until the
/// executor has taken the vault on: a key approved at its station for the
/// strategy's first driver. If it hasn't, say so with the likeliest reason
/// rather than wait on an agent that never starts.
async fn wait_taken_on(
    client: &Client,
    deployments: &Deployments,
    vault: alloy::primitives::Address,
    slug: &str,
    take_on: u64,
) -> Result<()> {
    let station = client.read(vault, IVault::homeStationCall {}).await?;
    let spec = client.read(station, IStation::specCall {}).await?;
    let Some(driver) = spec.drivers.first().copied() else {
        return Ok(());
    };
    let until = deadline(take_on);
    while tokio::time::Instant::now() < until {
        let key = client
            .read(station, IStation::approvedKeyCall { driver })
            .await?;
        if !key.is_zero() {
            tracing::info!(slug, %key, "the executor has taken the vault on");
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    let keeper = deployments.roles.keeper;
    let holds = client.balance(keeper).await.unwrap_or_default();
    bail!(
        "the executor has not taken {slug} ({vault}) on within {take_on} s: no key is approved at \
         its station, so its agent never starts. The vault's key is made by the executor, blessed \
         by a bless the keeper relays once the executor publishes a seal for it (in an enclave, \
         once the prover has proved its token), stocked with gas by the keeper once blessed, and \
         then approved at the station by the key itself. The executor's /attestation and log and \
         the keeper's log say which step is missing. The keeper holds {} of the chain's coin, and \
         stocks each vault key with three worst-case sends: if it is short, raise its amount in \
         config/funding.toml and run `task chains:fund NETWORK=testnet`. With a proof in the \
         path, allow longer with --take-on-seconds.",
        alloy::primitives::utils::format_ether(holds)
    )
}

/// How often, while the seed waits for the agent to put money to work, it
/// posts the stand-in venue's prices and funding again: each post is an
/// event the agent wakes to.
const NUDGE_EVERY: std::time::Duration = std::time::Duration::from_secs(20);

/// How long after the seeded withdrawal is read as done its claim is still
/// looked for, before the withdrawal is taken as paid from cash.
const CLAIM_SEEN_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

/// The funding rate the seed posts for the stand-in venue's perpetual while
/// it waits, as a fraction of the mark a day: five times what
/// `funding-basis` needs to start carrying (0.0002), and about what the local
/// market's own walk averages.
const NUDGED_FUNDING: (u64, u64) = (1, 1_000);

/// The depositor, once the agent has put part of the vault to work, asks for
/// all of it back: more than the vault's cash, so a claim is booked and the
/// keeper serves it. Fails if no claim is booked. Then the author puts a
/// little in, so the live vault keeps a depositor.
///
/// The leaver is the depositor, who holds nearly the whole vault, so their
/// withdrawal is more than the cash as soon as the agent has more than a
/// tenth of the vault at the venue, whatever share it aims for. (A leaver
/// who put money in after the agent moved would find it all in cash, since
/// an agent need not move again.)
///
/// Nothing moves the stand-in venue on a testnet, and `funding-basis` puts
/// money to work only when the venue's funding pays for it and the venue
/// tells it something happened. So while it waits the seed plays the market
/// operator for the run, as the local chain's market does every block:
/// it posts the venue's prices as they stand with a funding rate the agent
/// acts on. The withdrawal is asked for only once the station holds
/// positions at the venue and the vault's cash is under nine tenths of what
/// the depositor's shares are worth; a position isn't money the agent can
/// bring home before the next valuation, so the cash can't cover it and a
/// claim is booked.
async fn claim(
    client: &Client,
    deployments: &Deployments,
    vault: alloy::primitives::Address,
    slug: &str,
    take_on: u64,
) -> Result<()> {
    use chain::bindings::venue::ILocalVenue;
    let leaver = client.account(Role::Viewer)?;
    wait_taken_on(client, deployments, vault, slug, take_on).await?;
    let shares = wait_for_shares(client, vault, leaver).await?;
    tracing::info!(slug, %shares, "the deposit was priced at a later valuation");
    let station = client.read(vault, IVault::homeStationCall {}).await?;
    let mut at_work = false;
    let mut nudged: Option<tokio::time::Instant> = None;
    let until = deadline(WAIT_SECONDS);
    while tokio::time::Instant::now() < until {
        if nudged.is_none_or(|at| at.elapsed() >= NUDGE_EVERY) {
            if let Err(error) = nudge_the_market(client, deployments).await {
                tracing::warn!(error = %format!("{error:#}"), "the stand-in venue's prices weren't posted");
            } else if nudged.is_none() {
                tracing::info!(
                    slug,
                    "the seed posts the stand-in venue's prices and a funding rate the agent acts on, as its market operator"
                );
            }
            nudged = Some(tokio::time::Instant::now());
        }
        let held = client
            .read(
                deployments.venue,
                ILocalVenue::marketsHeldByCall { master: station },
            )
            .await?;
        let cash = client.read(vault, IVault::cashCall {}).await?;
        let price = client.read(vault, IVault::sharePriceCall {}).await?;
        let worth = shares * price / U256::from(10u64).pow(U256::from(18u64));
        if !held.is_empty() && cash * U256::from(10u64) < worth * U256::from(9u64) {
            at_work = true;
            tracing::info!(slug, %cash, %worth, markets = held.len(), "the agent has put the money to work");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    if !at_work {
        bail!(
            "the agent never put a tenth of the vault to work within {WAIT_SECONDS} s, so a withdrawal would be paid from cash and no claim booked"
        );
    }
    let claims_before = client
        .read(vault, IVault::claimCountCall {})
        .await?
        .to::<u64>();
    client
        .send(
            leaver,
            vault,
            IVault::requestRedeemCall {
                shares,
                receiver: leaver,
            },
        )
        .await
        .context("the seeded withdrawal")?;
    let request = client.read(vault, IVault::requestCountCall {}).await?;
    let mut booked = None;
    // The request can be read as done a moment before the claim it booked:
    // the two reads may land either side of the valuation, or on two
    // endpoints a block apart. So a done request is taken as paid from cash
    // only once the claim still isn't there a little later.
    let mut done_since: Option<tokio::time::Instant> = None;
    let until = deadline(WAIT_SECONDS);
    while tokio::time::Instant::now() < until {
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
            if claim.owner == leaver {
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
            let since = *done_since.get_or_insert_with(tokio::time::Instant::now);
            if since.elapsed() >= CLAIM_SEEN_WITHIN {
                bail!("the seeded withdrawal was paid from cash, so no claim was booked");
            }
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
    let until = deadline(WAIT_SECONDS);
    let mut paid = None;
    while tokio::time::Instant::now() < until {
        let claim = client
            .read(
                vault,
                IVault::claimAtCall {
                    claimId: U256::from(claim_id),
                },
            )
            .await?;
        if matches!(claim.state, ClaimState::Paid) {
            paid = Some(claim.paid);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let Some(paid) = paid else {
        bail!("a claim was booked but never paid within {WAIT_SECONDS} s: is the keeper running?");
    };
    tracing::info!(slug, %paid, "and the claim was paid");
    let author = client.account(Role::Author)?;
    deposit(client, deployments, vault, author, STAYING_DEPOSIT).await?;
    wait_for_shares(client, vault, author).await?;
    tracing::info!(
        slug,
        "and its author put a little in, so the vault keeps a depositor"
    );
    Ok(())
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
    take_on: u64,
) -> Result<()> {
    let viewer = client.account(Role::Viewer)?;
    wait_taken_on(client, deployments, vault, slug, take_on).await?;
    let shares = wait_for_shares(client, vault, viewer).await?;
    let until = deadline(WAIT_SECONDS);
    while tokio::time::Instant::now() < until {
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
    let until = deadline(WAIT_SECONDS);
    while tokio::time::Instant::now() < until {
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
