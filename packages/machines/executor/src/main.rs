//! The attested executor.
//!
//! It finds the vaults that are open, gets a key blessed for each one, has the
//! vault's station approve that key at the venue, and runs each vault's agent.
//! The key can trade and nothing else: the venue refuses a withdrawal signed by
//! it, and the station only moves money between the vault's own accounts.
//! Whatever an agent decides, that is the whole of what can happen.
//!
//! It also keeps each vault priced, valuing it every slot and settling what
//! the valuation prices, and keeps a signed running total of what running the
//! agent costs, for the keeper to collect from the tank.

mod feed;
mod health;
mod host;
mod runner;
mod scale;
mod usage;
mod watch;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::U256;
use alloy::primitives::{Address, B256, keccak256};
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result};
use axum::response::IntoResponse;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::IStation;
use chain::bindings::strategies::IStrategyRegistry;
use chain::bindings::vault::{IVault, VaultState};
use chain::{Client, Deployments};
use clap::Parser;
use wasmtime::Engine;

#[derive(Parser)]
#[command(about = "Runs strategies for the vaults it is blessed for")]
struct Args {
    #[command(flatten)]
    chain_args: chain::ChainArgs,
    /// Where bundles are, by hash.
    #[arg(long)]
    store: PathBuf,
    /// Where the health reports go.
    #[arg(long)]
    api_url: String,
    #[arg(long, default_value_t = 0)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    let served = args.chain_args.connect_all().await?;
    tracing::info!(image = %chain::attest::image_digest("executor"), "the executor is up");

    let totals: usage::Totals = Default::default();
    if args.port != 0 {
        tokio::spawn(serve_status(
            SocketAddr::from(([127, 0, 0, 1], args.port)),
            totals.clone(),
        ));
    }

    let engine = runner::engine()?;
    runner::start_epoch_ticker(&engine);

    // Each vault chain is swept on its own, with its own vaults: the same
    // address can be a different vault on another chain.
    let args = Arc::new(args);
    let mut chains = tokio::task::JoinSet::new();
    for served in served {
        let engine = engine.clone();
        let args = args.clone();
        let totals = totals.clone();
        chains.spawn(async move {
            let mut running: HashMap<Address, tokio::task::JoinHandle<()>> = HashMap::new();
            loop {
                if let Err(error) = sweep(
                    served.vault_chain,
                    &served.client,
                    &served.deployments,
                    &engine,
                    &args,
                    &totals,
                    &mut running,
                )
                .await
                {
                    tracing::warn!(chain = %served.key, %error, "could not look for work");
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }
    while chains.join_next().await.is_some() {}
    Ok(())
}

/// Find the vaults that are open and have a bundle, and start anything that
/// is not already running.
async fn sweep(
    chain: shared::Chain,
    client: &Client,
    deployments: &Deployments,
    engine: &Engine,
    args: &Args,
    totals: &usage::Totals,
    running: &mut HashMap<Address, tokio::task::JoinHandle<()>>,
) -> Result<()> {
    running.retain(|_, task| !task.is_finished());

    let vaults = client
        .read(deployments.factory, IVaultFactory::vaultsCall {})
        .await?;

    for vault in vaults {
        if running.contains_key(&vault) {
            continue;
        }
        let state = client.read(vault, IVault::stateCall {}).await?;
        // A retired vault still runs until it is evicted.
        if !matches!(state, VaultState::Open | VaultState::WindingDown)
            || finished(client, vault).await?
        {
            continue;
        }
        let bundle = client.read(vault, IVault::bundleHashCall {}).await?;
        if bundle == B256::ZERO {
            continue;
        }
        let component = bundle_path(&args.store, bundle);
        if !component.exists() {
            tracing::debug!(vault = %vault, "no bundle in the store yet");
            continue;
        }

        let client = client.clone();
        let deployments = deployments.clone();
        let engine = engine.clone();
        let store = args.store.clone();
        let api = args.api_url.clone();
        let network = args.chain_args.network.to_string();
        let totals = totals.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = serve(
                client,
                chain,
                deployments,
                engine,
                vault,
                bundle,
                store,
                api,
                network,
                totals,
            )
            .await
            {
                tracing::warn!(%chain, vault = %vault, error = format!("{error:#}"), "stopped serving this vault");
            }
        });
        running.insert(vault, task);
        tracing::info!(%chain, vault = %vault, "taking on a vault");
    }
    Ok(())
}

/// Run one vault's strategy, restarting it if it faults.
#[allow(clippy::too_many_arguments)]
async fn serve(
    chain: Client,
    vault_chain: shared::Chain,
    deployments: Deployments,
    engine: Engine,
    vault: Address,
    bundle: B256,
    store: PathBuf,
    api: String,
    network: String,
    totals: usage::Totals,
) -> Result<()> {
    // The key is made here and never written down. Deriving it from the image
    // and the vault means a restart picks the same one back up, so the
    // approval the vault already granted still stands.
    let signer = key_for(vault);
    let key = chain.with_key(signer)?;

    // A fresh key has no gas of its own, and it pays for its own blessing
    // from here on: one account per vault, so two vaults never reach for the
    // same next nonce.
    health::fund_gas(&chain, &deployments, key.address).await?;

    let info = chain::attest::bless_for_vault(
        &chain,
        &deployments,
        "executor",
        key.address,
        chain::attest::Sender::Own(Box::new(key.clone())),
        vault,
        bundle,
    )
    .await?;
    tracing::info!(vault = %vault, key = %key.address, until = info.blessedUntil, "blessed");

    // The station is the venue account's master and approves the key there.
    // Anyone may ask; it obeys only what the registry says.
    let station = chain.read(vault, IVault::homeStationCall {}).await?;
    key.send(
        station,
        IStation::approveKeyCall {
            driver: chain::names::LOCAL_DRIVER,
            key: key.address,
        },
    )
    .await
    .context("the station would not approve the key")?;

    // Keep it blessed, and keep the venue's approval following the blessing:
    // a key that stopped attesting stops being able to trade.
    let approving = key.clone();
    let _renewing = AbortOnDrop(chain::attest::keep_blessed(
        chain.clone(),
        deployments.clone(),
        "executor",
        key.address,
        chain::attest::Sender::Own(Box::new(key.clone())),
        vault,
        bundle,
        move || {
            let key = approving.clone();
            tokio::spawn(async move {
                if let Err(error) = key
                    .send(
                        station,
                        IStation::approveKeyCall {
                            driver: chain::names::LOCAL_DRIVER,
                            key: key.address,
                        },
                    )
                    .await
                {
                    tracing::warn!(%vault, %error, "the venue's approval did not follow");
                }
            });
        },
    ));

    // A strategy's `run` is meant never to return, so the report cannot wait
    // for it: the counters are shared and sent while it is still running, and
    // they outlive a restart so that restarts can be counted at all.
    let counters = Arc::new(Mutex::new(host::Counters::default()));
    let reporter = health::Reporter::new(chain.clone(), vault_chain, key.clone(), vault, api);
    let reporting = counters.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            let now = reporting
                .lock()
                .map(|counters| *counters)
                .unwrap_or_default();
            reporter.post(now).await;
        }
    });

    // The executor reads the manifest the builder recorded, and refuses one
    // naming a driver this image doesn't carry.
    let strategy_id = chain.read(vault, IVault::strategyIdCall {}).await?;
    let registered = chain
        .read(
            deployments.strategies,
            IStrategyRegistry::strategyOfCall {
                strategyId: strategy_id,
            },
        )
        .await?;
    let driver = driver_for(&store, registered.manifestHash, &network)?;

    // What running it costs, kept as one signed total, and whether the tank
    // can still pay for it.
    let dry = Arc::new(AtomicBool::new(false));
    tokio::spawn(usage::run(
        chain.clone(),
        key.clone(),
        vault,
        totals,
        dry.clone(),
    ));
    // Value the vault every slot and settle what each valuation prices.
    tokio::spawn(price(chain.clone(), key.clone(), vault));

    // An agent that stops is started again. It rebuilds its own picture from
    // the venue, so a restart costs it nothing but the time.
    loop {
        if dry.load(Ordering::SeqCst) {
            if finished(&chain, vault).await? {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        // The author's allowances as they stand: a change applies from the
        // agent's next start.
        let allowances =
            usage::shared_allowances(&chain.read(vault, IVault::allowancesCall {}).await?);
        let (account_tx, account_rx) = tokio::sync::mpsc::unbounded_channel();
        let (vault_tx, vault_rx) = tokio::sync::mpsc::unbounded_channel();
        let from_block = chain.block_number().await?;
        let watching = tokio::spawn(watch::run(
            chain.clone(),
            vault,
            station,
            deployments.venue,
            from_block,
            account_tx,
            vault_tx,
        ));

        let runner = runner::Runner {
            chain: chain.clone(),
            key: key.clone(),
            vault,
            station,
            venue: deployments.venue,
            usd: deployments.usd,
            factory: deployments.factory,
            home_chain: vault_chain.name().to_string(),
            driver: driver.clone(),
            allowances,
        };
        let component = bundle_path(&store, bundle);
        let stopped_dry = tokio::select! {
            outcome = runner.run(
                &engine,
                &component,
                account_rx,
                vault_rx,
                counters.clone(),
            ) => {
                if let Err(error) = outcome {
                    tracing::warn!(%vault, %error, "the agent could not be run");
                }
                false
            }
            // A dry tank stops the agent where it is, positions left as they
            // are, until the author tops it up.
            _ = until(dry.clone()) => true,
            // Once the grace is over the station has stopped, and the agent
            // is let go wherever it is.
            _ = until_finished(&chain, vault) => false,
        };
        watching.abort();

        // Once the vault has halted and its grace is over, the station is
        // stopped and nothing is left to run.
        if finished(&chain, vault).await? {
            tracing::info!(%vault, "the vault has stopped; letting it go");
            return Ok(());
        }
        if stopped_dry {
            continue;
        }
        // An agent that ends once it was told the vault is halting has done
        // its part; it waits out the grace instead of starting again.
        let halted = chain
            .read(
                vault,
                chain::bindings::station::IStationVault::haltedAtCall {},
            )
            .await?;
        if halted != 0 {
            tracing::info!(%vault, "the agent ended while the vault halts");
            while !finished(&chain, vault).await? {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            return Ok(());
        }
        if let Ok(mut counters) = counters.lock() {
            counters.restarts += 1;
        }
        tracing::warn!(%vault, "restarting the strategy");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// Stops the task it holds when the vault it serves is let go, however
/// `serve` returns.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Resolve when the flag is set.
async fn until(flag: Arc<AtomicBool>) {
    while !flag.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Resolve once the vault has nothing left to run.
async fn until_finished(chain: &Client, vault: Address) {
    loop {
        if matches!(finished(chain, vault).await, Ok(true)) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Whether there is nothing left to run: the vault never opened or has
/// closed, or it halted and the agent's grace is over.
async fn finished(chain: &Client, vault: Address) -> Result<bool> {
    let state = chain.read(vault, IVault::stateCall {}).await?;
    if matches!(
        state,
        VaultState::None | VaultState::Closed | VaultState::Cancelled
    ) {
        return Ok(true);
    }
    let halted = chain
        .read(
            vault,
            chain::bindings::station::IStationVault::haltedAtCall {},
        )
        .await?;
    if halted == 0 {
        return Ok(false);
    }
    let grace = chain
        .read(
            vault,
            chain::bindings::station::IStationVault::haltGraceCall {},
        )
        .await?;
    Ok(chain.block_timestamp().await? >= halted + grace)
}

/// Value the vault at every slot, and settle what each valuation prices.
/// Anyone may; doing it here means a vault is never waiting on somebody else
/// to be priced.
async fn price(chain: Client, key: chain::KeyClient, vault: Address) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Err(error) = price_once(&chain, &key, vault).await {
            tracing::debug!(%vault, error = format!("{error:#}"), "could not price the vault");
        }
        if matches!(finished(&chain, vault).await, Ok(true)) {
            let state = chain.read(vault, IVault::stateCall {}).await;
            if matches!(state, Ok(VaultState::Closed | VaultState::Cancelled)) {
                return;
            }
        }
    }
}

async fn price_once(chain: &Client, key: &chain::KeyClient, vault: Address) -> Result<()> {
    let timers = chain.read(vault, IVault::timersCall {}).await?;
    let now = chain.block_timestamp().await?;
    let slot = now - now % timers.slotInterval;
    let count = chain.read(vault, IVault::valuationCountCall {}).await?;
    let last = if count.is_zero() {
        0
    } else {
        chain
            .read(
                vault,
                IVault::valuationAtCall {
                    index: count - U256::from(1u64),
                },
            )
            .await?
            .time
    };
    if now - slot <= timers.slotWindow && last < slot {
        // Settle what the last valuation priced first: a new one waits for it.
        let _ = key
            .send(
                vault,
                IVault::settleCall {
                    max: U256::from(100u64),
                },
            )
            .await;
        key.send(vault, IVault::recordValuationCall {})
            .await
            .context("recording a valuation")?;
    }
    key.send(
        vault,
        IVault::settleCall {
            max: U256::from(100u64),
        },
    )
    .await
    .context("settling")?;
    Ok(())
}

/// The driver an agent trades through, from the manifest the builder
/// recorded. A manifest naming a driver this image doesn't carry, or one not
/// offered on this network, is refused.
fn driver_for(store: &std::path::Path, manifest_hash: B256, network: &str) -> Result<String> {
    let hash = format!("sha256:{}", alloy::hex::encode(manifest_hash));
    let path = store.join(hash.replace(':', "-")).join("manifest.json");
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let canonical: shared::Canonical =
        serde_json::from_slice(&bytes).context("the recorded manifest")?;
    // The store is only believed where its bytes match what the chain holds.
    anyhow::ensure!(
        canonical.hash() == hash,
        "the stored manifest is not the one the registry recorded"
    );
    let catalogue = shared::Catalogue::published();
    canonical
        .manifest
        .check(&catalogue, network)
        .map_err(|why| anyhow::anyhow!("this image won't run it: {why}"))?;
    // This image carries one driver's executor half: the stand-in venue's.
    let drivers: Vec<&String> = canonical.manifest.drivers.keys().collect();
    match drivers.as_slice() {
        [only] if only.as_str() == "local" => Ok((*only).clone()),
        _ => anyhow::bail!(
            "this image carries the local driver only, and the manifest names {drivers:?}"
        ),
    }
}

/// Where a bundle's component lives once the build service has written it.
fn bundle_path(store: &std::path::Path, bundle: B256) -> PathBuf {
    store
        .join(format!("sha256-{}", alloy::hex::encode(bundle)))
        .join("component.wasm")
}

/// A key for one vault, made where the strategy runs.
fn key_for(vault: Address) -> PrivateKeySigner {
    let seed = keccak256((chain::attest::image_digest("executor"), vault).abi_encode_seed());
    PrivateKeySigner::from_bytes(&seed).expect("a key from a seed")
}

/// A small helper so the seed reads as one expression.
trait Seed {
    fn abi_encode_seed(&self) -> Vec<u8>;
}

impl Seed for (B256, Address) {
    fn abi_encode_seed(&self) -> Vec<u8> {
        let mut out = self.0.to_vec();
        out.extend_from_slice(self.1.as_slice());
        out
    }
}

/// The machine's own health, and each vault's signed running total for the
/// keeper to collect.
async fn serve_status(address: SocketAddr, totals: usage::Totals) {
    let app = axum::Router::new()
        .route("/health", axum::routing::get(|| async { "executor" }))
        .route(
            "/usage/{chain}/{vault}",
            axum::routing::get(
                move |axum::extract::Path((chain, vault)): axum::extract::Path<(u64, Address)>| {
                    let totals = totals.clone();
                    async move {
                        let found = totals
                            .lock()
                            .ok()
                            .and_then(|totals| totals.get(&(chain, vault)).cloned());
                        match found {
                            Some(signed) => axum::Json(signed).into_response(),
                            None => axum::http::StatusCode::NOT_FOUND.into_response(),
                        }
                    }
                },
            ),
        );
    if let Ok(listener) = tokio::net::TcpListener::bind(address).await {
        let _ = axum::serve(listener, app).await;
    }
}
