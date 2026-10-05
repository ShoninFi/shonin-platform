//! The seed on a Solana cluster: a strategy published, vaults activated,
//! deposits priced, money put to work at the stand-in venue, a claim booked
//! and paid, a vault retired and one killed and closed with its depositor
//! paid. Each outcome is read back from the cluster, not from a log.
//!
//! Until the services serve Solana, the seed plays their parts itself, each
//! with that role's development key: the build service and the Reviewer
//! register and review, the executor approves its key at the station and
//! trades through the guard, and the keeper values, settles, serves claims
//! and winds the killed vault up. Every one of those is the same instruction
//! the service will send, built by `chain::solana`.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chain::solana::platform::{Declared, Registration, VaultAccounts};
use chain::solana::programs::{local_venue, prices, publishing, registry, vault};
use chain::solana::{Keypair, Pubkey, Signer, SolanaClient, TransactionFailed, name};
use chain::{Role, SolanaServed};

use vault::state::{ClaimState, VaultState};

/// What the seeded depositor puts in.
const DEPOSIT: u64 = 25_000_000_000;
/// What the author puts in and asks back, to book a claim.
const CLAIM_DEPOSIT: u64 = 20_000_000_000;
/// A whole unit of size at the venue.
const UNIT: u128 = 1_000_000_000_000_000_000;
/// How long a step is waited on before the seed gives up on it.
const PATIENCE: Duration = Duration::from_secs(180);

/// Set (to anything) when the keeper is running: the seed then sends none of
/// the keeper's calls itself (valuations, settlement, claims, the wind-up)
/// and waits for the keeper to make them.
const KEEPER_RUNS: &str = "SHONIN_SEED_KEEPER_RUNS";

fn keeper_runs() -> bool {
    std::env::var_os(KEEPER_RUNS).is_some_and(|value| !value.is_empty())
}

struct Seed<'a> {
    served: &'a SolanaServed,
    client: &'a SolanaClient,
    platform: &'a chain::solana::Platform,
    nonce: std::cell::Cell<u64>,
}

pub async fn run(served: &SolanaServed) -> Result<()> {
    let seed = Seed {
        served,
        client: &served.client,
        platform: &served.platform,
        nonce: std::cell::Cell::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |since| since.as_nanos() as u64),
        ),
    };
    seed.say_who_the_author_is().await?;
    seed.wait_for_the_timelock().await?;

    // Published, activated, a depositor in, money at work, a claim paid.
    let live = seed.publish("Funding basis on Solana").await?;
    let v = seed.activate(&live).await?;
    let viewer = seed.client.role(Role::Viewer)?;
    seed.deposit(&v, viewer, DEPOSIT).await?;
    seed.wait_for_shares(&v, &viewer.pubkey()).await?;
    tracing::info!(vault = %v.vault, "and a depositor's money was priced");
    seed.read_a_valuation(&v).await?;
    seed.put_to_work(&v, &live, DEPOSIT / 2).await?;
    seed.claim(&v, &live).await?;

    // Published and activated, a depositor in, then retired by its author.
    let retiring = seed.publish("Funding basis on Solana, retired").await?;
    let r = seed.activate(&retiring).await?;
    seed.deposit(&r, viewer, DEPOSIT).await?;
    seed.wait_for_shares(&r, &viewer.pubkey()).await?;
    let author = seed.client.role(Role::Author)?;
    seed.client
        .send(
            vec![seed.platform.ix_retire(&author.pubkey(), &r.vault)],
            &[author],
        )
        .await
        .context("the vault would not retire")?;
    let state = seed.platform.vault(seed.client, &r.vault).await?.state;
    if state != VaultState::WindingDown {
        bail!("a retired vault is {state:?}, not winding down");
    }
    tracing::info!(vault = %r.vault, "and then its author retired it");

    // Published and activated, a depositor's money at work, then killed:
    // the station stopped, everything brought home, the vault closed and the
    // depositor paid.
    let stopping = seed.publish("Funding basis on Solana, stopped").await?;
    let k = seed.activate(&stopping).await?;
    seed.deposit(&k, viewer, DEPOSIT).await?;
    let shares = seed.wait_for_shares(&k, &viewer.pubkey()).await?;
    seed.put_to_work(&k, &stopping, DEPOSIT / 2).await?;
    seed.killed(&k, shares).await?;

    let count = seed.platform.publishing_config(seed.client).await?.count;
    tracing::info!(strategies = count, "the Solana cluster has something on it");
    Ok(())
}

/// A strategy as registered and reviewed.
struct Published {
    id: u64,
    bundle: [u8; 32],
}

impl Seed<'_> {
    fn next_nonce(&self) -> u64 {
        let nonce = self.nonce.get() + 1;
        self.nonce.set(nonce);
        nonce
    }

    fn keeper(&self) -> Result<&Keypair> {
        self.client.role(Role::Keeper)
    }

    async fn say_who_the_author_is(&self) -> Result<()> {
        let author = self.client.role(Role::Author)?;
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

    /// The images and the stand-in driver are proposed when the cluster is
    /// set up, and usable once the timelock has passed on its clock.
    async fn wait_for_the_timelock(&self) -> Result<()> {
        let started = std::time::Instant::now();
        let mut said = false;
        loop {
            let now = self.client.now().await?;
            let builder: Option<registry::state::ImageRecord> = self
                .client
                .load(&registry::read::image_address(
                    &chain::solana::platform::image_digest("builder"),
                ))
                .await?;
            let slot: Option<registry::state::ApprovalSlot> = self
                .client
                .load(&registry::read::approval_address(
                    registry::state::ApprovalKind::Driver,
                    &chain::solana::programs::local_driver::NAME,
                ))
                .await?;
            let images = registry::state::is_image_allowed(builder.as_ref(), now);
            let driver = slot.is_some_and(|slot| {
                slot.effective(now) == chain::solana::programs::local_driver::ID
            });
            if images && driver {
                return Ok(());
            }
            if !said {
                tracing::info!(
                    "waiting for the registry's timelock to pass on the cluster's clock: is the devnet running?"
                );
                said = true;
            }
            if started.elapsed() > PATIENCE {
                bail!("the registry's timelock never passed: is the devnet driving the clock?");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    /// Paid for, registered with the build service's signature and the
    /// Reviewer's go-ahead, and reviewed.
    async fn publish(&self, title: &str) -> Result<Published> {
        let author = self.client.role(Role::Author)?;
        let builder = self.client.role(Role::Builder)?;
        let reviewer = self.client.role(Role::Reviewer)?;
        let attestor = self.client.role(Role::Attestor)?;
        let digest = |part: &str| {
            chain::solana::programs::common::digest::keccak(&[part.as_bytes(), title.as_bytes()])
        };

        // A blessing lasts three days, a few minutes on the fast clock: the
        // build service and the Reviewer attest again before each job.
        for (service, key) in [("builder", builder), ("reviewer", reviewer)] {
            self.platform
                .bless_service(self.client, key, attestor, service, &key.pubkey())
                .await
                .with_context(|| format!("blessing the {service}"))?;
        }

        let (submit, id) = self
            .platform
            .ix_submit(self.client, &author.pubkey(), digest("upload"))
            .await?;
        self.client
            .send(vec![submit], &[author])
            .await
            .context("paying to publish")?;

        let registration = Registration {
            id,
            author: author.pubkey(),
            source_hash: digest("source"),
            bundle_hash: digest("bundle"),
            manifest_hash: digest("manifest"),
            lists: publishing::state::Lists {
                chains: vec![publishing::state::ChainNeed {
                    chain: name("solana"),
                    drivers: vec![chain::solana::programs::local_driver::NAME],
                }],
                bridges: vec![],
                restricted: vec![name("SANCTIONED"), name("US")],
            },
        };
        self.platform
            .register(self.client, builder, &registration, builder, reviewer)
            .await
            .context("registering the strategy")?;
        let review = publishing::state::ReviewInput {
            bundle_hash: registration.bundle_hash,
            source_hash: registration.source_hash,
            model_id_hash: digest("model"),
            prompt_hash: digest("prompt"),
            report_hash: digest("report"),
            ratings: [3; 6],
            finding_count: 0,
        };
        let ix = self
            .platform
            .ix_submit_review(
                self.client,
                &reviewer.pubkey(),
                &reviewer.pubkey(),
                id,
                review,
            )
            .await?;
        self.client
            .send(vec![ix], &[reviewer])
            .await
            .context("posting the review")?;

        let record = self
            .platform
            .strategy(self.client, id)
            .await?
            .ok_or_else(|| anyhow!("strategy {id} was registered and is not on the cluster"))?;
        if record.author != author.pubkey() || record.bundle_hash != registration.bundle_hash {
            bail!("strategy {id} is not what was registered");
        }
        if !self
            .platform
            .review(self.client, id)
            .await?
            .is_some_and(|review| review.is_review())
        {
            bail!("strategy {id} was reviewed and has no review");
        }
        tracing::info!(title, id, "published");
        Ok(Published {
            id,
            bundle: registration.bundle_hash,
        })
    }

    /// The author activates the strategy's vault on the stand-in driver,
    /// with the least tank the terms allow, and the executor's key is
    /// blessed for it.
    async fn activate(&self, published: &Published) -> Result<VaultAccounts> {
        let author = self.client.role(Role::Author)?;
        let drivers = [Declared::local()];
        let (_, statement) = self
            .platform
            .restricted_for(self.client, &author.pubkey(), published.id, &drivers)
            .await?;
        let factory = self.platform.factory(self.client).await?;
        let terms = vault::factory::terms_hash(&factory.terms);
        let ix = self
            .platform
            .ix_activate(
                self.client,
                &author.pubkey(),
                published.id,
                &drivers,
                // Nothing the tank pays for: the seed runs weeks of the fast
                // clock, and no executor bills this vault yet.
                prices::Allowances::default(),
                500,
                factory.terms.minimum_tank,
                statement,
                terms,
            )
            .await?;
        self.client
            .send(vec![ix], &[author])
            .await
            .context("activating the vault")?;
        let v = self.platform.vault_accounts(published.id);
        let state = self.platform.vault(self.client, &v.vault).await?.state;
        if state != VaultState::Open {
            bail!("an activated vault is {state:?}, not open");
        }
        tracing::info!(vault = %v.vault, id = published.id, "and activated");
        Ok(v)
    }

    /// `who` asks to deposit, stating for the vault's list as it is.
    async fn deposit(&self, v: &VaultAccounts, who: &Keypair, amount: u64) -> Result<()> {
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

    /// What the keeper does on each pass: this slot's valuation, if one is
    /// due, and every settlement it owes.
    async fn keep(&self, v: &VaultAccounts) -> Result<()> {
        if keeper_runs() {
            return Ok(());
        }
        let keeper = self.keeper()?;
        match self.platform.value(self.client, keeper, v).await {
            Ok(_) => {}
            // Outside the slot's window, or this slot already valued: the
            // next pass tries again.
            Err(error) if error.downcast_ref::<TransactionFailed>().is_some() => {
                tracing::debug!(error = format!("{error:#}"), "no valuation this pass");
            }
            Err(error) => return Err(error),
        }
        self.platform.settle_all(self.client, keeper, v).await?;
        Ok(())
    }

    /// Keep the vault until `done` says so.
    async fn keep_until<F, Fut>(&self, v: &VaultAccounts, what: &str, mut done: F) -> Result<()>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<bool>>,
    {
        let started = std::time::Instant::now();
        loop {
            if done().await? {
                return Ok(());
            }
            if started.elapsed() > PATIENCE {
                bail!("{what} never happened: is the devnet driving the clock?");
            }
            self.keep(v).await?;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// A valuation recorded, and its figures read back from the event the
    /// vault emitted as a call of its own: what an indexer rebuilds the
    /// record from.
    async fn read_a_valuation(&self, v: &VaultAccounts) -> Result<()> {
        let keeper = self.keeper()?;
        let started = std::time::Instant::now();
        loop {
            if keeper_runs() {
                let vault = self.platform.vault(self.client, &v.vault).await?;
                if vault.valuation_count > 0 {
                    tracing::info!(vault = %v.vault, valuations = vault.valuation_count, "and the keeper values it");
                    return Ok(());
                }
            } else if let Ok(landed) = self.platform.value(self.client, keeper, v).await {
                let valued = landed
                    .record
                    .cpi_events::<vault::events::Valued>(&vault::ID);
                let vault = self.platform.vault(self.client, &v.vault).await?;
                let [(_, event)] = valued.as_slice() else {
                    bail!(
                        "a valuation landed with {} Valued events, not one",
                        valued.len()
                    );
                };
                if event.index + 1 != vault.valuation_count || event.supply == 0 {
                    bail!("the Valued event doesn't match the vault it valued");
                }
                tracing::info!(vault = %v.vault, index = event.index, gross = event.gross, fee = landed.fee(), "and a valuation's figures were read from its event");
                return Ok(());
            }
            if started.elapsed() > PATIENCE {
                bail!("no valuation could be recorded: is the devnet driving the clock?");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn wait_for_shares(&self, v: &VaultAccounts, who: &Pubkey) -> Result<u128> {
        self.keep_until(v, "a deposit priced", || async {
            Ok(self.platform.shares(self.client, &v.vault, who).await? > 0)
        })
        .await?;
        self.platform.shares(self.client, &v.vault, who).await
    }

    /// The executor, blessed for the vault, approves its key at the
    /// station, moves `amount` of the vault's cash there and through the
    /// guard to the venue, and buys spot with most of it.
    async fn put_to_work(
        &self,
        v: &VaultAccounts,
        published: &Published,
        amount: u64,
    ) -> Result<()> {
        let executor = self.client.role(Role::Executor)?;
        let attestor = self.client.role(Role::Attestor)?;
        let key = executor.pubkey();
        let bundle = &published.bundle;
        self.platform
            .bless_executor(self.client, executor, attestor, &key, &v.vault, bundle)
            .await
            .context("blessing the executor for the vault")?;
        let station = self.platform.station(self.client, &v.station).await?;
        if station.drivers[0].key != key {
            let ix = self
                .platform
                .ix_approve_key(self.client, v, bundle, &key, 0, &key)
                .await?;
            self.client
                .send(vec![ix], &[executor])
                .await
                .context("approving the executor at the station")?;
        }
        let ix = self
            .platform
            .ix_move(self.client, v, bundle, &key, amount, true)
            .await?;
        self.client
            .send(vec![ix], &[executor])
            .await
            .context("moving the vault's cash to its station")?;

        let station = self.platform.station(self.client, &v.station).await?;
        let authority = station.drivers[0].authority;
        if self
            .platform
            .margin_account(self.client, &authority)
            .await?
            .is_none()
        {
            let open = self.platform.venue_open(&key, &authority);
            let ix = self
                .platform
                .ix_execute(self.client, v, bundle, &key, 0, open, 0)
                .await?;
            self.client
                .send(vec![ix], &[executor])
                .await
                .context("opening the station's account at the venue")?;
        }
        let deposit = self.platform.venue_deposit(&authority, amount);
        let ix = self
            .platform
            .ix_execute(self.client, v, bundle, &key, 0, deposit, amount)
            .await?;
        self.client
            .send(vec![ix], &[executor])
            .await
            .context("depositing at the venue through the guard")?;

        let market: local_venue::Market = self
            .client
            .load(&local_venue::market_address(&name(super::solana::SPOT)))
            .await?
            .ok_or_else(|| anyhow!("the venue has no spot market"))?;
        let limit = market.ask + market.ask / 100;
        let size = (amount as u128 * 8 / 10) * UNIT / limit as u128;
        let order = local_venue::OrderRequest {
            market: name(super::solana::SPOT),
            is_buy: true,
            size,
            limit_price: limit,
            reduce_only: false,
        };
        let place = self
            .platform
            .venue_place(self.client, &authority, vec![order])
            .await?;
        let ix = self
            .platform
            .ix_execute(self.client, v, bundle, &key, 0, place, 0)
            .await?;
        self.client
            .send(vec![ix], &[executor])
            .await
            .context("buying spot through the guard")?;
        let held = self
            .platform
            .margin_account(self.client, &authority)
            .await?
            .map_or(0, |account| account.size_in(&name(super::solana::SPOT)));
        if held <= 0 {
            bail!("the station bought spot and holds none");
        }
        tracing::info!(vault = %v.vault, amount, "and the agent put money to work at the venue");
        Ok(())
    }

    /// The author deposits, the agent puts the vault's cash to work, and the
    /// author asks for all of it back: more than the vault's cash, so a claim
    /// is booked, and the keeper serves it.
    async fn claim(&self, v: &VaultAccounts, published: &Published) -> Result<()> {
        let author = self.client.role(Role::Author)?;
        self.deposit(v, author, CLAIM_DEPOSIT).await?;
        self.wait_for_shares(v, &author.pubkey()).await?;
        let cash = self.platform.vault(self.client, &v.vault).await?.cash;
        self.put_to_work(v, published, cash * 9 / 10).await?;

        let shares = self
            .platform
            .shares(self.client, &v.vault, &author.pubkey())
            .await?;
        let before = self
            .client
            .token_balance(&self.platform.dollars(&author.pubkey()))
            .await?;
        let ix = self.platform.ix_request_redeem(
            &v.vault,
            &author.pubkey(),
            shares,
            &author.pubkey(),
            self.next_nonce(),
        );
        self.client
            .send(vec![ix], &[author])
            .await
            .context("the seeded withdrawal")?;
        let request = self
            .platform
            .vault(self.client, &v.vault)
            .await?
            .request_count;

        // Priced at the next valuation: paid from cash, or booked as a claim.
        self.keep_until(v, "the withdrawal priced", || async {
            if self
                .platform
                .claim(self.client, &v.vault, request)
                .await?
                .is_some()
            {
                return Ok(true);
            }
            let vault = self.platform.vault(self.client, &v.vault).await?;
            Ok(vault.request_cursor >= request)
        })
        .await?;
        if self
            .platform
            .claim(self.client, &v.vault, request)
            .await?
            .is_none()
        {
            bail!("the withdrawal was paid from cash, so no claim was booked");
        }
        tracing::info!(vault = %v.vault, claim = request, "and a withdrawal larger than the cash booked a claim");

        let keeper = self.keeper()?;
        self.keep_until(v, "the claim paid", || async {
            let claim = self
                .platform
                .claim(self.client, &v.vault, request)
                .await?
                .ok_or_else(|| anyhow!("claim {request} is gone"))?;
            match claim.state {
                ClaimState::Paid => return Ok(true),
                ClaimState::Pending | ClaimState::Waiting if !keeper_runs() => {
                    let ix = self
                        .platform
                        .ix_settle_claim(self.client, v, &keeper.pubkey(), request)
                        .await?;
                    if let Err(error) = self.client.send(vec![ix], &[keeper]).await {
                        tracing::debug!(
                            error = format!("{error:#}"),
                            "the claim was not served this pass"
                        );
                    }
                }
                _ => {}
            }
            Ok(false)
        })
        .await?;
        let claim = self
            .platform
            .claim(self.client, &v.vault, request)
            .await?
            .ok_or_else(|| anyhow!("claim {request} is gone"))?;
        let after = self
            .client
            .token_balance(&self.platform.dollars(&author.pubkey()))
            .await?;
        if claim.paid == 0 || after <= before {
            bail!("claim {request} is paid and its owner received nothing");
        }
        tracing::info!(vault = %v.vault, paid = claim.paid, "and the claim was paid");
        Ok(())
    }

    /// The admin stops the vault with the depositor's money at work; once
    /// the halt's grace has passed the keeper winds it up; the depositor is
    /// paid from the closed vault.
    async fn killed(&self, v: &VaultAccounts, shares: u128) -> Result<()> {
        let admin = self.client.role(Role::Admin)?;
        let viewer = self.client.role(Role::Viewer)?;
        self.client
            .send(
                vec![self.platform.ix_kill(&admin.pubkey(), &v.vault)],
                &[admin],
            )
            .await
            .context("the vault would not stop")?;
        tracing::info!(vault = %v.vault, "and then stopped with a depositor's money at work");

        let keeper = self.keeper()?;
        let started = std::time::Instant::now();
        loop {
            let vault = self.platform.vault(self.client, &v.vault).await?;
            if vault.state == VaultState::Closed {
                break;
            }
            if !keeper_runs()
                && self.client.now().await? >= vault.halted_at + vault.timers.halt_grace
            {
                match self.platform.wind_up(self.client, keeper, v).await {
                    Ok(()) => continue,
                    Err(error) => {
                        tracing::debug!(error = format!("{error:#}"), "not wound up this pass");
                    }
                }
            }
            if started.elapsed() > PATIENCE {
                bail!("a stopped vault never closed: is the devnet driving the clock?");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        let to = self.platform.dollars(&viewer.pubkey());
        let before = self.client.token_balance(&to).await?;
        let ix = self
            .platform
            .ix_redeem_closed(v, &viewer.pubkey(), shares, &viewer.pubkey());
        self.client
            .send(vec![ix], &[viewer])
            .await
            .context("the depositor's withdrawal from the closed vault")?;
        let paid = self.client.token_balance(&to).await? - before;
        tracing::info!(vault = %v.vault, paid, "and closed with its cash home; the depositor was paid");
        // Paid in full, less the transaction fee, fees for the time it ran
        // and what the market did meanwhile.
        if paid < DEPOSIT / 100 * 95 {
            bail!("the depositor of a closed vault was paid {paid} of {DEPOSIT}");
        }
        let _ = self.served;
        Ok(())
    }
}
