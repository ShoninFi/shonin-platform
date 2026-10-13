//! The seed and the demo on a public Solana cluster, through the platform's
//! own flows, as on an EVM testnet (see [`crate::seed`]).
//!
//! An agent is packed and uploaded to the API, paid for from the author's
//! own dollar account, reviewed by the cloud reviewer and built and
//! registered by the cloud build service with their own Solana keys. Its
//! vault is then activated by its author, a depositor's money priced at a
//! later valuation by the keeper, put to work by the agent at the stand-in
//! venue, and all of it asked back, booking a claim the keeper serves. Each
//! step is read back from the cluster. Nothing here signs as a service: the
//! seed holds the author's, the depositor's and the market operator's keys,
//! and the admin's only to mint the stand-in dollar, as anyone may on an EVM
//! testnet.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chain::solana::platform::VaultAccounts;
use chain::solana::programs::{local_venue, prices, vault};
use chain::solana::{Keypair, Pubkey, Signer, SolanaClient, name};
use chain::{Role, SolanaServed};
use shared::model::registry::JobState;
use shared::{Allowances, Chain, Upload};
use vault::state::ClaimState;

use crate::seed::{
    CLAIM_SEEN_WITHIN, DEPOSIT, NUDGE_EVERY, NUDGED_FUNDING, STAYING_DEPOSIT, WAIT_SECONDS,
    deadline,
};

/// A strategy published on the cluster.
struct Published {
    job_id: String,
    outcome: JobState,
    strategy_id: Option<u64>,
}

/// The agent to publish on Solana: its own Solana copy beside it
/// (`<agent>/solana`) if it has one, or else a copy of it whose manifest names
/// Solana as its vault chain.
fn project_for(agents: &Path, name: &str) -> Result<(PathBuf, Option<PathBuf>)> {
    let project = agents.join("packages/agents").join(name);
    let own = project.join(Chain::Solana.name());
    if own.join("shonin.toml").exists() {
        return Ok((own, None));
    }
    if !project.join("shonin.toml").exists() {
        bail!("no agent at {}", project.display());
    }
    let moved = crate::seed::on_vault_chain(agents, &project, Chain::Solana)?;
    Ok((project, moved))
}

fn agents_dir(agents: Option<&Path>) -> Result<PathBuf> {
    agents
        .map(Path::to_path_buf)
        .or_else(crate::seed::guess_agents)
        .context("where the agents live: pass --agents or set SHONIN_AGENTS_DIR")
}

/// The seed's flows on the cluster.
struct Seed<'a> {
    client: &'a SolanaClient,
    platform: &'a chain::solana::Platform,
    nonce: std::cell::Cell<u64>,
}

/// Publish `name` once (and, unless `publish_only`, activate it, and with
/// `live` use its vault as the EVM seed's live vault is used).
#[allow(clippy::too_many_arguments)]
pub async fn one(
    served: &SolanaServed,
    api: &str,
    agents: Option<&Path>,
    name: &str,
    live: bool,
    publish_only: bool,
    take_on: u64,
) -> Result<()> {
    let seed = Seed::new(served);
    let agents = agents_dir(agents)?;
    let (project, moved) = project_for(&agents, name)?;
    let project = moved.as_deref().unwrap_or(&project);
    let author = seed.client.role(Role::Author)?;
    seed.say_who_the_author_is(author).await?;
    tracing::info!(project = %project.display(), "publishing on Solana");
    let published = seed.publish(api, project, author).await?;
    let id = match published.outcome {
        JobState::Ready => published.strategy_id.context("a ready job has an id")?,
        other => bail!("job {} ended {}", published.job_id, other.label()),
    };
    let slug = shared::Strategy::slug_for(Chain::Solana, id);
    if publish_only {
        tracing::info!(
            slug,
            job = published.job_id,
            "published; no vault activated"
        );
        // For the task that runs the live part later.
        println!("strategy {id}");
        return Ok(());
    }
    let v = seed.activate(id, author).await?;
    tracing::info!(slug, vault = %v.vault, "published and activated");
    if live {
        seed.used_live(&v, &slug, take_on).await?;
    }
    Ok(())
}

/// The live part of [`one`] on a strategy already published: its vault
/// activated if it has none yet, then a depositor in, a claim booked and
/// paid, and a little of the author's left in.
pub async fn on_strategy(served: &SolanaServed, strategy: u64, take_on: u64) -> Result<()> {
    let seed = Seed::new(served);
    if seed
        .platform
        .strategy(seed.client, strategy)
        .await?
        .is_none()
    {
        bail!("strategy {strategy} is not registered on this cluster");
    }
    let slug = shared::Strategy::slug_for(Chain::Solana, strategy);
    let author = seed.client.role(Role::Author)?;
    let v = seed.platform.vault_accounts(strategy);
    if seed.client.account(&v.vault).await?.is_none() {
        seed.activate(strategy, author).await?;
        tracing::info!(slug, vault = %v.vault, "activated");
    } else {
        tracing::info!(slug, vault = %v.vault, "its vault is already activated");
    }
    seed.used_live(&v, &slug, take_on).await
}

/// The demo on the cluster (see [`crate::seed::demo`]): a fresh vault,
/// activated at the modest allowances, its tank filled to and kept at
/// `tank_days` of the daily cap by auto-fill, a depositor's money in and at
/// work, then left running.
pub async fn demo(served: &SolanaServed, demo: &crate::seed::Demo<'_>) -> Result<()> {
    let seed = Seed::new(served);
    let author = seed.client.role(Role::Author)?;
    let fee = seed.platform.publishing_config(seed.client).await?.fee;
    let factory = seed.platform.factory(seed.client).await?;
    seed.have(author, fee + factory.terms.minimum_tank).await?;
    let strategy = match demo.strategy {
        Some(id) => {
            if seed.platform.strategy(seed.client, id).await?.is_none() {
                bail!("strategy {id} is not registered on this cluster");
            }
            let v = seed.platform.vault_accounts(id);
            if seed.client.account(&v.vault).await?.is_some() {
                bail!(
                    "strategy {id} already has its vault ({}): a demo runs on a fresh one, so publish again (--agent) or pass a strategy published with --publish-only",
                    v.vault
                );
            }
            id
        }
        None => {
            let agents = agents_dir(demo.agents)?;
            let (project, moved) = project_for(&agents, demo.agent)?;
            let project = moved.as_deref().unwrap_or(&project);
            seed.say_who_the_author_is(author).await?;
            tracing::info!(project = %project.display(), "publishing on Solana, for a fresh vault");
            let published = seed.publish(demo.api, project, author).await?;
            match published.outcome {
                JobState::Ready => published.strategy_id.context("a ready job has an id")?,
                other => bail!("job {} ended {}", published.job_id, other.label()),
            }
        }
    };
    let slug = shared::Strategy::slug_for(Chain::Solana, strategy);
    let v = seed.activate(strategy, author).await?;
    tracing::info!(slug, vault = %v.vault, "activated at the modest allowances");

    let daily = seed
        .platform
        .daily_cost(seed.client, &modest())
        .await
        .context("the allowances' daily cost")?;
    let keep = daily.saturating_mul(demo.tank_days);
    let tank = seed
        .platform
        .vault(seed.client, &v.vault)
        .await?
        .tank
        .balance;
    let keep = keep.max(tank);
    let more = keep - tank;
    if more > 0 {
        seed.have(author, more).await?;
        seed.client
            .send(
                vec![seed.platform.ix_fund_tank(&author.pubkey(), &v.vault, more)],
                &[author],
            )
            .await
            .context("filling the tank")?;
    }
    seed.client
        .send(
            vec![
                seed.platform
                    .ix_set_tank_keep(&author.pubkey(), &v.vault, keep),
            ],
            &[author],
        )
        .await
        .context("turning auto-fill on")?;
    tracing::info!(
        slug,
        tank = keep,
        daily_cap = daily,
        days = demo.tank_days,
        "the tank is filled, and auto-fill keeps it there from the author's fee shares"
    );

    let viewer = seed.client.role(Role::Viewer)?;
    let amount = demo.deposit_dollars * 1_000_000;
    seed.have(viewer, amount).await?;
    seed.deposit(&v, viewer, amount).await?;
    tracing::info!(
        slug,
        dollars = demo.deposit_dollars,
        "a depositor asked to put money in"
    );
    seed.wait_taken_on(&v, &slug, demo.take_on).await?;
    let shares = seed.wait_for_shares(&v, &viewer.pubkey()).await?;
    tracing::info!(slug, shares, "the deposit was priced at a later valuation");
    if !seed.at_work(&v, amount, demo.work_within, false).await? {
        let perp = seed.perp().await?;
        bail!(
            "the agent of {slug} ({}) has not put the money to work within {} min. The venue quotes funding of {:.6} a day, last posted at {}: funding-basis enters at 0.0002. Is the market operator posting on Solana (task chains:market)? The vault is left as it is.",
            v.vault,
            demo.work_within.as_secs() / 60,
            perp.funding_rate as f64 / perp.mark.max(1) as f64,
            perp.funding_updated_at
        );
    }
    println!("vault {}", v.vault);
    println!(
        "The demo is running and is left to run: {slug} ({}) on Solana, ${} deposited, the tank at {} (auto-fill on).",
        v.vault,
        demo.deposit_dollars,
        keep as f64 / 1e6
    );
    println!(
        "Keep the market operator posting (task chains:market NETWORK={}), and the services up.",
        demo.network
    );
    Ok(())
}

/// The modest allowances, as the Solana price list reads them: the gas
/// allowance in the dollar's units a day.
fn modest() -> prices::Allowances {
    let modest = Allowances::modest();
    prices::Allowances {
        fuel_per_sec: modest.fuel_per_sec,
        memory_mib: modest.memory_mib,
        requests_per_min: modest.requests_per_min,
        bytes_per_day: modest.bytes_per_day,
        streams: modest.streams,
        inference_ms_per_min: modest.inference_ms_per_min,
        gas_per_day: (modest.gas_per_day.max(0.0) * 1_000_000.0).round() as u64,
    }
}

impl<'a> Seed<'a> {
    fn new(served: &'a SolanaServed) -> Self {
        Seed {
            client: &served.client,
            platform: &served.platform,
            nonce: std::cell::Cell::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(1, |since| since.as_nanos() as u64),
            ),
        }
    }

    fn next_nonce(&self) -> u64 {
        let nonce = self.nonce.get() + 1;
        self.nonce.set(nonce);
        nonce
    }

    async fn say_who_the_author_is(&self, author: &Keypair) -> Result<()> {
        if self
            .platform
            .profile(self.client, &author.pubkey())
            .await?
            .is_some_and(|profile| !profile.handle.is_empty())
        {
            return Ok(());
        }
        let ix = self
            .platform
            .ix_set_profile(
                self.client,
                &author.pubkey(),
                "shonin",
                "The platform's own reference agents. They are published like anyone \
                 else's, and their records are as public as anyone else's.",
            )
            .await?;
        self.client.send(vec![ix], &[author]).await?;
        Ok(())
    }

    /// Make sure `who` holds `amount` of the stand-in dollar, the admin
    /// minting what it lacks.
    async fn have(&self, who: &Keypair, amount: u64) -> Result<()> {
        let held = self
            .client
            .token_balance(&self.platform.dollars(&who.pubkey()))
            .await
            .unwrap_or(0);
        if held >= amount {
            return Ok(());
        }
        let admin = self.client.role(Role::Admin)?;
        let ix = self
            .platform
            .ix_mint_to(&admin.pubkey(), &who.pubkey(), amount - held)?;
        self.client
            .send(vec![ix], &[admin])
            .await
            .context("minting the stand-in dollar")?;
        Ok(())
    }

    /// Upload the project to the API, pay for it from the author's dollars,
    /// and follow the job until it is ready, refused or failed.
    async fn publish(&self, api: &str, project: &Path, author: &Keypair) -> Result<Published> {
        let archive = crate::publish::pack(project)?;
        let accepted = crate::publish::upload(
            api,
            Chain::Solana,
            &Upload {
                author: author.pubkey().to_string(),
                archive_base64: shared::base64::encode(&archive),
                sealed: false,
                package: crate::publish::package_of(project),
            },
        )
        .await?;
        let reference: [u8; 32] = alloy::hex::decode(&accepted.upload)
            .context("the upload's reference is not hex")?
            .try_into()
            .map_err(|_| anyhow!("the upload's reference is not 32 bytes"))?;
        let fee = self.platform.publishing_config(self.client).await?.fee;
        self.have(author, fee).await?;
        // The submission takes the next id: one paid by someone else in
        // between is a submission at another address, and this is sent again.
        let mut tries = 0;
        let id = loop {
            let (ix, id) = self
                .platform
                .ix_submit(self.client, &author.pubkey(), reference)
                .await?;
            match self.client.send(vec![ix], &[author]).await {
                Ok(_) => break id,
                Err(error) if tries < 3 => {
                    tries += 1;
                    tracing::info!(error = format!("{error:#}"), "paying again");
                }
                Err(error) => return Err(error).context("the fee could not be paid"),
            }
        };
        tracing::info!(id, job = accepted.job_id, "paid; the job is queued");
        let job = crate::publish::follow(api, &accepted.job_id).await?;
        Ok(Published {
            job_id: accepted.job_id,
            outcome: job.state,
            strategy_id: job.strategy_id,
        })
    }

    /// Once its review is posted, the author activates the strategy's vault
    /// on the drivers its record names, at the modest allowances, with the least tank
    /// the terms allow.
    async fn activate(&self, id: u64, author: &Keypair) -> Result<VaultAccounts> {
        let until = deadline(WAIT_SECONDS);
        loop {
            if self
                .platform
                .review(self.client, id)
                .await?
                .is_some_and(|review| review.is_review())
            {
                break;
            }
            if tokio::time::Instant::now() >= until {
                bail!("the review of {id} was never posted");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let drivers = self.platform.declared_drivers(self.client, id).await?;
        let (_, statement) = self
            .platform
            .restricted_for(self.client, &author.pubkey(), id, &drivers)
            .await?;
        let factory = self.platform.factory(self.client).await?;
        self.have(author, factory.terms.minimum_tank).await?;
        let terms = vault::factory::terms_hash(&factory.terms);
        let ix = self
            .platform
            .ix_activate(
                self.client,
                &author.pubkey(),
                id,
                &drivers,
                modest(),
                500,
                factory.terms.minimum_tank,
                statement,
                terms,
            )
            .await?;
        self.client
            .send(vec![ix], &[author])
            .await
            .context("the vault could not be activated")?;
        Ok(self.platform.vault_accounts(id))
    }

    /// `who` asks to deposit, stating for the vault's list as it is.
    async fn deposit(&self, v: &VaultAccounts, who: &Keypair, amount: u64) -> Result<()> {
        self.have(who, amount).await?;
        let statement = self
            .platform
            .vault(self.client, &v.vault)
            .await?
            .restricted_hash;
        let ix = self.platform.ix_request_deposit(
            &v.vault,
            &who.pubkey(),
            &who.pubkey(),
            amount,
            Some(statement),
            self.next_nonce(),
        );
        self.client
            .send(vec![ix], &[who])
            .await
            .context("the seeded deposit")?;
        Ok(())
    }

    /// Wait until a deposit has been priced and `who` holds shares.
    async fn wait_for_shares(&self, v: &VaultAccounts, who: &Pubkey) -> Result<u128> {
        let until = deadline(WAIT_SECONDS);
        while tokio::time::Instant::now() < until {
            let held = self.platform.shares(self.client, &v.vault, who).await?;
            if held > 0 {
                return Ok(held);
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        bail!("a deposit was never priced within {WAIT_SECONDS} s: is the keeper running?")
    }

    /// Wait, for at most `take_on` seconds, until the executor has taken the
    /// vault on: a key approved at its station.
    async fn wait_taken_on(&self, v: &VaultAccounts, slug: &str, take_on: u64) -> Result<()> {
        let until = deadline(take_on);
        while tokio::time::Instant::now() < until {
            let station = self.platform.station(self.client, &v.station).await?;
            if let Some(key) = station
                .drivers
                .first()
                .map(|driver| driver.key)
                .filter(|key| *key != Pubkey::default())
            {
                tracing::info!(slug, %key, "the executor has taken the vault on");
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        let keeper = self.client.address(Role::Keeper)?;
        let holds = self.client.lamports(&keeper).await.unwrap_or_default();
        bail!(
            "the executor has not taken {slug} ({}) on within {take_on} s: no key is approved at \
             its station, so its agent never starts. The vault's key is made by the executor, \
             blessed by a bless the keeper relays once the prover has proved the executor's \
             token, stocked with lamports by the keeper once blessed, and then approved at the \
             station by the key itself. The executor's /attestation and log and the keeper's \
             log say which step is missing. The keeper holds {} SOL. With a proof in the path, \
             allow longer with --take-on-seconds.",
            v.vault,
            holds as f64 / 1e9
        )
    }

    /// The venue's perpetual.
    async fn perp(&self) -> Result<local_venue::Market> {
        self.client
            .load(&local_venue::market_address(&name(crate::solana::PERP)))
            .await?
            .ok_or_else(|| anyhow!("the venue has no perpetual"))
    }

    /// Post the venue's prices as they stand, with a funding rate on its
    /// perpetual of [`NUDGED_FUNDING`] of the mark a day, as its market
    /// operator.
    async fn nudge_the_market(&self) -> Result<()> {
        let operator = self.client.role(Role::Operator)?;
        let mut quotes = Vec::new();
        let mut funding = Vec::new();
        for market in [crate::solana::SPOT, crate::solana::PERP] {
            let held: local_venue::Market = self
                .client
                .load(&local_venue::market_address(&name(market)))
                .await?
                .ok_or_else(|| anyhow!("the venue has no {market} market"))?;
            quotes.push(local_venue::Quote {
                market: held.id,
                mark: held.mark,
                oracle: held.oracle,
                bid: held.bid,
                ask: held.ask,
            });
            if held.is_perp {
                funding.push(local_venue::Funding {
                    market: held.id,
                    rate_per_day: (held.mark as i128) * NUDGED_FUNDING.0 as i128
                        / NUDGED_FUNDING.1 as i128,
                    paid_per_unit: 0,
                });
            }
        }
        self.client
            .send(vec![self.platform.ix_post(quotes, funding)], &[operator])
            .await
            .context("posting the stand-in venue's prices")?;
        Ok(())
    }

    /// Whether the agent has put the money to work within `within`: its
    /// station holds a position at the venue and the vault's cash is under
    /// nine tenths of `deposited`. With `nudge`, the seed posts the venue's
    /// prices as its market operator every [`NUDGE_EVERY`] meanwhile.
    async fn at_work(
        &self,
        v: &VaultAccounts,
        deposited: u64,
        within: Duration,
        nudge: bool,
    ) -> Result<bool> {
        let station = self.platform.station(self.client, &v.station).await?;
        let authority = station
            .drivers
            .first()
            .map(|driver| driver.authority)
            .context("the station has no driver")?;
        let mut nudged: Option<tokio::time::Instant> = None;
        let until = tokio::time::Instant::now() + within;
        while tokio::time::Instant::now() < until {
            if nudge && nudged.is_none_or(|at| at.elapsed() >= NUDGE_EVERY) {
                if let Err(error) = self.nudge_the_market().await {
                    tracing::warn!(error = %format!("{error:#}"), "the stand-in venue's prices weren't posted");
                } else if nudged.is_none() {
                    tracing::info!(
                        "the seed posts the stand-in venue's prices and a funding rate the agent acts on, as its market operator"
                    );
                }
                nudged = Some(tokio::time::Instant::now());
            }
            let held = self
                .platform
                .margin_account(self.client, &authority)
                .await?
                .is_some_and(|account| {
                    account.size_in(&name(crate::solana::SPOT)) != 0
                        || account.size_in(&name(crate::solana::PERP)) != 0
                });
            let cash = self.platform.vault(self.client, &v.vault).await?.cash;
            if held && u128::from(cash) * 10 < u128::from(deposited) * 9 {
                tracing::info!(vault = %v.vault, cash, "the agent has put the money to work");
                return Ok(true);
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        Ok(false)
    }

    /// A depositor in, the agent at work, all of it asked back so a claim is
    /// booked and the keeper pays it, then a little of the author's left in.
    async fn used_live(&self, v: &VaultAccounts, slug: &str, take_on: u64) -> Result<()> {
        let leaver = self.client.role(Role::Viewer)?;
        self.deposit(v, leaver, DEPOSIT).await?;
        tracing::info!(slug, "a depositor asked to put money in");
        self.wait_taken_on(v, slug, take_on).await?;
        let shares = self.wait_for_shares(v, &leaver.pubkey()).await?;
        tracing::info!(slug, shares, "the deposit was priced at a later valuation");
        if !self
            .at_work(v, DEPOSIT, Duration::from_secs(WAIT_SECONDS), true)
            .await?
        {
            bail!(
                "the agent never put a tenth of the vault to work within {WAIT_SECONDS} s, so a withdrawal would be paid from cash and no claim booked"
            );
        }
        let to = self.platform.dollars(&leaver.pubkey());
        let before = self.client.token_balance(&to).await?;
        let ix = self.platform.ix_request_redeem(
            &v.vault,
            &leaver.pubkey(),
            shares,
            &leaver.pubkey(),
            self.next_nonce(),
        );
        self.client
            .send(vec![ix], &[leaver])
            .await
            .context("the seeded withdrawal")?;
        let request = self
            .platform
            .vault(self.client, &v.vault)
            .await?
            .request_count;

        // Priced at the next valuation: paid from cash, or booked as a claim.
        // A request read as done a moment before its claim is taken as paid
        // from cash only once the claim still isn't there a little later.
        let mut done_since: Option<tokio::time::Instant> = None;
        let until = deadline(WAIT_SECONDS);
        let booked = loop {
            if let Some(claim) = self.platform.claim(self.client, &v.vault, request).await? {
                break claim;
            }
            let vault = self.platform.vault(self.client, &v.vault).await?;
            if vault.request_cursor >= request {
                let since = *done_since.get_or_insert_with(tokio::time::Instant::now);
                if since.elapsed() >= CLAIM_SEEN_WITHIN {
                    bail!("the seeded withdrawal was paid from cash, so no claim was booked");
                }
            }
            if tokio::time::Instant::now() >= until {
                bail!(
                    "the seeded withdrawal booked no claim within {WAIT_SECONDS} s: is the keeper running?"
                );
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        };
        tracing::info!(
            slug,
            claim = request,
            shares = booked.shares,
            "and a withdrawal larger than the cash booked a claim"
        );

        let until = deadline(WAIT_SECONDS);
        let paid = loop {
            let claim = self
                .platform
                .claim(self.client, &v.vault, request)
                .await?
                .ok_or_else(|| anyhow!("claim {request} is gone"))?;
            if claim.state == ClaimState::Paid {
                break claim.paid;
            }
            if tokio::time::Instant::now() >= until {
                bail!(
                    "a claim was booked but never paid within {WAIT_SECONDS} s: is the keeper running?"
                );
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        };
        let after = self.client.token_balance(&to).await?;
        let received = after.saturating_sub(before);
        if paid == 0 || received == 0 {
            bail!("claim {request} is paid ({paid}) and its owner received {received}");
        }
        tracing::info!(
            slug,
            paid,
            received,
            "and the claim was paid, in the dollar's base units, as the cluster records it"
        );

        let author = self.client.role(Role::Author)?;
        self.deposit(v, author, STAYING_DEPOSIT).await?;
        self.wait_for_shares(v, &author.pubkey()).await?;
        tracing::info!(
            slug,
            "and its author put a little in, so the vault keeps a depositor"
        );
        Ok(())
    }
}

// The market operator ---------------------------------------------------------

/// Play the stand-in venue's market on the cluster in real time, as the
/// operator, from the venue's prices as they stand (see
/// [`crate::market::operate`]): every `every`, both markets' prices from a
/// tape like bitcoin's, and the funding the rate quoted last accrued since,
/// by the cluster's clock. The venue's reserves are kept at what the set-up
/// gave them, the admin minting what they have paid out beyond; and Orca's
/// pool is brought back to Pyth's price whenever it has strayed.
pub async fn operate(served: &SolanaServed, every: Duration) -> Result<()> {
    use crate::market::{Tape, TapeParams};
    let client = &served.client;
    let platform = &served.platform;
    let operator = client.role(Role::Operator)?;
    if operator.pubkey() != platform.operator {
        return Err(anyhow::Error::new(WrongOperator {
            ours: operator.pubkey(),
            venue: platform.operator,
        }));
    }
    let load = |market: &'static str| async move {
        client
            .load::<local_venue::Market>(&local_venue::market_address(&name(market)))
            .await?
            .ok_or_else(|| anyhow!("the venue has no {market} market"))
    };
    let spot = load(crate::solana::SPOT).await?;
    let perp = load(crate::solana::PERP).await?;
    let params = TapeParams::TESTNET;
    let basis = if spot.mark > 0 && perp.mark > 0 && perp.mark != spot.mark {
        (perp.mark as f64 / spot.mark as f64 - 1.0).clamp(-0.01, 0.01)
    } else {
        params.basis_mean
    };
    let now = client.now().await?;
    let mut tape = Tape::with(params, now, spot.mark as f64, basis);
    tracing::info!(
        cluster = %served.key,
        minutes = every.as_secs_f64() / 60.0,
        spot = spot.mark as f64 / 1e6,
        "the market operator is running on Solana: a post each interval, from the venue's prices as they stand"
    );
    let mut timer = tokio::time::interval(every);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last = tokio::time::Instant::now();
    loop {
        timer.tick().await;
        let hours = last.elapsed().as_secs_f64().max(1.0) / 3_600.0;
        last = tokio::time::Instant::now();
        let tick = tape.step_by(hours);
        let posted = async {
            let perp = load(crate::solana::PERP).await?;
            let now = client.now().await?;
            let elapsed = now
                .saturating_sub(perp.funding_updated_at)
                .min(crate::market::LONGEST_ACCRUAL);
            let paid = paid_per_unit(perp.funding_rate, elapsed);
            keep_reserves(served).await?;
            let quote = |market: &str, price: f64| {
                let mark = price.max(1.0) as u64;
                let half = tape.half_spread(price) as u64;
                local_venue::Quote {
                    market: name(market),
                    mark,
                    oracle: mark,
                    bid: mark.saturating_sub(half),
                    ask: mark + half,
                }
            };
            client
                .send(
                    vec![platform.ix_post(
                        vec![
                            quote(crate::solana::SPOT, tick.spot),
                            quote(crate::solana::PERP, tick.perp),
                        ],
                        vec![local_venue::Funding {
                            market: name(crate::solana::PERP),
                            rate_per_day: tick.rate_per_day as i128,
                            paid_per_unit: paid,
                        }],
                    )],
                    &[operator],
                )
                .await
                .context("posting the venue's prices")?;
            Ok::<_, anyhow::Error>(paid)
        }
        .await;
        match posted {
            Ok(paid) => tracing::info!(
                cluster = %served.key,
                spot = format!("{:.2}", tick.spot / 1e6),
                perp = format!("{:.2}", tick.perp / 1e6),
                funding_a_year = format!("{:.2}%", tick.funding_fraction() * 36_500.0),
                paid_per_unit = paid,
                "posted"
            ),
            Err(error) => {
                tracing::warn!(cluster = %served.key, error = %format!("{error:#}"), "the post did not land; the next one accrues for this stretch too")
            }
        }
        // Orca's pool, brought back to Pyth's price if it has strayed: on a
        // testnet nobody else arbitrages it.
        let kept = async {
            let admin = client.role(Role::Admin)?;
            crate::solana_orca::keep_with_pyth(client, platform, admin, operator).await
        };
        if let Err(error) = kept.await {
            tracing::warn!(cluster = %served.key, error = %format!("{error:#}"), "Orca's pool could not be brought to Pyth's price; tried again next time");
        }
    }
}

/// The mnemonic here is not the one the cluster was set up with: no retry
/// changes that.
#[derive(Debug)]
pub(crate) struct WrongOperator {
    ours: Pubkey,
    venue: Pubkey,
}

impl std::fmt::Display for WrongOperator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this key's operator is {}, and the venue's is {}: the mnemonic is not the one this cluster was set up with",
            self.ours, self.venue
        )
    }
}

impl std::error::Error for WrongOperator {}

/// What one unit of the perpetual pays over `elapsed` seconds at
/// `rate_per_day`, rounded towards zero.
fn paid_per_unit(rate_per_day: i128, elapsed: u64) -> i128 {
    rate_per_day * i128::from(elapsed) / i128::from(crate::market::DAY)
}

/// Keep the venue's reserves at what the set-up gave them: what it has paid
/// out in funding and profits beyond them is minted back by the admin.
async fn keep_reserves(served: &SolanaServed) -> Result<()> {
    let client = &served.client;
    let short = crate::solana::VENUE_RESERVES
        .saturating_sub(client.token_balance(&local_venue::vault_address()).await?);
    if short == 0 {
        return Ok(());
    }
    let admin = client.role(Role::Admin)?;
    client
        .send(
            vec![served.platform.ix_mint_into(
                &admin.pubkey(),
                &local_venue::vault_address(),
                short,
            )?],
            &[admin],
        )
        .await
        .context("the venue's reserves")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_modest_allowances_read_the_same_on_solana() {
        let solana = modest();
        let modest = Allowances::modest();
        assert_eq!(solana.fuel_per_sec, modest.fuel_per_sec);
        assert_eq!(solana.memory_mib, modest.memory_mib);
        assert_eq!(solana.gas_per_day, 1_000_000);
    }

    #[test]
    fn funding_accrues_the_rate_over_the_time_elapsed_rounded_towards_zero() {
        let rate = 18_000_000i128;
        assert_eq!(paid_per_unit(rate, 600), 125_000);
        assert_eq!(paid_per_unit(rate, 86_400), rate);
        assert_eq!(paid_per_unit(-rate, 1), -208);
    }

    #[test]
    fn an_agent_with_its_own_solana_copy_is_published_from_it() {
        let root = std::env::temp_dir().join(format!("shonin-agents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let agent = root.join("packages/agents/funding-basis");
        std::fs::create_dir_all(agent.join("solana")).unwrap();
        std::fs::write(agent.join("shonin.toml"), "[vault]\nchain = \"base\"\n").unwrap();
        std::fs::write(
            agent.join("solana/shonin.toml"),
            "[vault]\nchain = \"solana\"\n",
        )
        .unwrap();
        let (project, moved) = project_for(&root, "funding-basis").unwrap();
        assert_eq!(project, agent.join("solana"));
        assert!(moved.is_none());
        assert!(project_for(&root, "nobody").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
