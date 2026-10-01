//! The attested executor.
//!
//! It finds the vaults that are open, gets a key blessed for each one, gets
//! that key approved on the venue, and runs each vault's strategy. The key it
//! holds can trade and nothing else: the venue refuses a withdrawal signed by
//! it, and the vault's contract will only move money between its own two
//! accounts. Whatever a strategy decides, that is the whole of what can
//! happen.

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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::{Address, B256, keccak256};
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result};
use chain::bindings::factory::IVaultFactory;
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

    if args.port != 0 {
        tokio::spawn(serve_status(SocketAddr::from(([127, 0, 0, 1], args.port))));
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
        chains.spawn(async move {
            let mut running: HashMap<Address, tokio::task::JoinHandle<()>> = HashMap::new();
            loop {
                if let Err(error) = sweep(
                    served.vault_chain,
                    &served.client,
                    &served.deployments,
                    &engine,
                    &args,
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
        if !matches!(state, VaultState::Open) {
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
        chain::attest::Sender::Own(key.clone()),
        vault,
        bundle,
    )
    .await?;
    tracing::info!(vault = %vault, key = %key.address, until = info.blessedUntil, "blessed");

    // Anyone may send this; the contract obeys only what the registry says.
    key.send(
        vault,
        IVault::approveKeyCall {
            venue: chain::accounts::VENUE_ID,
            key: key.address,
        },
    )
    .await
    .context("the vault would not approve the key")?;

    // Keep it blessed, and keep the venue's approval following the blessing:
    // a key that stopped attesting stops being able to trade.
    let approving = key.clone();
    chain::attest::keep_blessed(
        chain.clone(),
        deployments.clone(),
        "executor",
        key.address,
        chain::attest::Sender::Own(key.clone()),
        vault,
        bundle,
        move || {
            let key = approving.clone();
            tokio::spawn(async move {
                if let Err(error) = key
                    .send(
                        vault,
                        IVault::approveKeyCall {
                            venue: chain::accounts::VENUE_ID,
                            key: key.address,
                        },
                    )
                    .await
                {
                    tracing::warn!(%vault, %error, "the venue's approval did not follow");
                }
            });
        },
    );

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
    // The vault holds its author's allowances as a daily cap; until it records
    // the allowances themselves, the host holds every agent to the modest
    // shape.
    let allowances = shared::Allowances::modest();

    // What the day cost, said once a day and charged to the author's tank.
    tokio::spawn(usage::run(chain.clone(), key.clone(), vault, allowances));

    // A strategy that stops is started again. It rebuilds its own picture from
    // the venue, so a restart costs it nothing but the time.
    loop {
        let (account_tx, account_rx) = tokio::sync::mpsc::unbounded_channel();
        let (vault_tx, vault_rx) = tokio::sync::mpsc::unbounded_channel();
        let from_block = chain.block_number().await?;
        let watching = tokio::spawn(watch::run(
            chain.clone(),
            vault,
            deployments.venue,
            from_block,
            account_tx,
            vault_tx,
        ));

        let runner = runner::Runner {
            chain: chain.clone(),
            key: key.clone(),
            vault,
            venue: deployments.venue,
            driver: driver.clone(),
            allowances,
        };
        let outcome = runner
            .run(
                &engine,
                &bundle_path(&store, bundle),
                account_rx,
                vault_rx,
                counters.clone(),
            )
            .await;
        watching.abort();
        if let Err(error) = outcome {
            tracing::warn!(%vault, %error, "the strategy could not be run");
        }

        // The vault may have been killed or frozen while it ran, in which case
        // there is nothing left to restart.
        let state = chain.read(vault, IVault::stateCall {}).await?;
        if !matches!(state, VaultState::Open) {
            tracing::info!(%vault, "the vault is no longer open; letting it go");
            return Ok(());
        }
        if let Ok(mut counters) = counters.lock() {
            counters.restarts += 1;
        }
        tracing::warn!(%vault, "restarting the strategy");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
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

async fn serve_status(address: SocketAddr) {
    let app = axum::Router::new().route("/health", axum::routing::get(|| async { "executor" }));
    if let Ok(listener) = tokio::net::TcpListener::bind(address).await {
        let _ = axum::serve(listener, app).await;
    }
}
