//! The development chains.
//!
//! It deploys the contracts to every EVM chain of the local network, funds
//! the accounts the platform's roles use, moves each chain's clock faster than
//! real time, and plays the market each venue quotes. Everything else on the
//! platform reads the files it writes and talks to the chains it prepared.

mod deploy;
mod market;
mod publish;
mod seed;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Result, bail};
use chain::{
    ChainArgs, ChainConfig, ChainKey, ChainKind, Client, Deployments, NetworkName, Target,
};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Deploys and drives the local development chains")]
struct Args {
    #[command(flatten)]
    chain_args: ChainArgs,
    /// The content-addressed store the services share.
    #[arg(long)]
    store: PathBuf,
    /// The compiled contracts, as the build tool leaves them.
    #[arg(long)]
    artifacts: PathBuf,
    /// Where to answer "are you up?".
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Seconds of chain time each block advances.
    #[arg(long, default_value_t = 3_600)]
    seconds_per_block: u64,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Deploy the contracts to every EVM chain and write the addresses, then
    /// stop.
    Deploy,
    /// Publish the reference agents through the platform's own flows, on the
    /// vault chain.
    Seed {
        /// Where the API is answering.
        #[arg(long)]
        api: String,
        /// Where the strategies to publish live.
        #[arg(long, env = "SHONIN_AGENTS_DIR")]
        agents: Option<PathBuf>,
        /// Publish only this agent, once, at the version its manifest names,
        /// instead of the full set of examples the site needs.
        #[arg(long)]
        agent: Option<String>,
    },
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
    let target = args.chain_args.resolve()?;
    // The devnet signs with the accounts a development chain unlocks, which
    // no public chain has.
    if target.network != NetworkName::Local {
        bail!(
            "the devnet drives the local network only, and this is {}",
            target.network
        );
    }

    match args.command {
        Some(Command::Deploy) => {
            for (key, config) in evm_chains(&target) {
                let client = Client::connect(&config).await?;
                let file = Deployments::file_in(&target.deployments_dir, &key);
                deploy::ensure(&client, &key, &args.artifacts, &file).await?;
            }
            Ok(())
        }
        Some(Command::Seed { api, agents, agent }) => {
            let client = target.connect().await?;
            let deployments = client
                .wait_for_deployments(&target.deployments_file())
                .await?;
            match agent {
                Some(name) => {
                    seed::one(&client, &deployments, &api, agents.as_deref(), &name).await
                }
                None => {
                    seed::run(&client, &deployments, &api, agents.as_deref(), &args.store).await
                }
            }
        }
        None => {
            let mut running = tokio::task::JoinSet::new();
            for (key, config) in evm_chains(&target) {
                let client = Client::connect(&config).await?;
                let file = Deployments::file_in(&target.deployments_dir, &key);
                let deployments = deploy::ensure(&client, &key, &args.artifacts, &file).await?;
                tracing::info!(chain = %key, label = config.label, "the chain is up");
                let seconds_per_block = args.seconds_per_block;
                running.spawn(async move {
                    market::run(&client, &deployments, seconds_per_block)
                        .await
                        .map_err(|error| error.context(format!("the market on {key}")))
                });
            }
            if args.port != 0 {
                let address = SocketAddr::from(([127, 0, 0, 1], args.port));
                tokio::spawn(serve_status(address));
            }
            while let Some(finished) = running.join_next().await {
                finished??;
            }
            Ok(())
        }
    }
}

/// Every EVM chain of the network, which is every chain the contracts go on.
fn evm_chains(target: &Target) -> Vec<(ChainKey, ChainConfig)> {
    let Some(network) = target.configs.network(target.network) else {
        return Vec::new();
    };
    let mut chains: Vec<(ChainKey, ChainConfig)> = network
        .iter()
        .filter(|(_, key)| key.kind == ChainKind::Evm)
        .filter_map(|(_, key)| {
            let config = target.configs.get(key)?;
            Some((key.clone(), config.clone()))
        })
        .collect();
    chains.sort_by(|a, b| a.0.cmp(&b.0));
    chains.dedup_by(|a, b| a.0 == b.0);
    chains
}

/// A service answers "are you up?" so the tasks that start them all at once
/// have something to wait on.
async fn serve_status(address: SocketAddr) {
    let app = axum::Router::new().route("/health", axum::routing::get(|| async { "devnet" }));
    match tokio::net::TcpListener::bind(address).await {
        Ok(listener) => {
            let _ = axum::serve(listener, app).await;
        }
        Err(error) => tracing::warn!(%error, "no status endpoint"),
    }
}
