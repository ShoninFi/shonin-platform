//! The development chains, and the ops dashboard.
//!
//! On the local network it deploys the contracts to every EVM chain, funds
//! the accounts the platform's roles use, moves each chain's clock faster than
//! real time, and plays the market each venue quotes. Everything else on the
//! platform reads the files it writes and talks to the chains it prepared.
//!
//! On any network it serves the ops dashboard on its port: what is running,
//! locally and in the cloud.

mod dashboard;
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
    /// Where to serve the dashboard and answer "are you up?".
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// A service for the dashboard to watch, as name=url, where the URL
    /// answers when it is up. Repeat it for each one.
    #[arg(long = "service", value_parser = name_and_value)]
    services: Vec<(String, String)>,
    /// A Google Cloud project for the dashboard to list, as network=project.
    /// An empty project is skipped: nothing is ever read from the gcloud
    /// default.
    #[arg(long = "gcp-project", value_parser = name_and_value)]
    gcp_projects: Vec<(String, String)>,
    /// The region the Cloud Run services are listed in.
    #[arg(long)]
    gcp_region: Option<String>,
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
    // no public chain has. On any other network it only shows what is there.
    let local = target.network == NetworkName::Local;
    if !local && args.command.is_some() {
        bail!(
            "the devnet deploys and seeds the local network only, and this is {}",
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
            if args.port != 0 {
                let address = SocketAddr::from(([127, 0, 0, 1], args.port));
                let gcp_projects = args
                    .gcp_projects
                    .into_iter()
                    .filter(|(_, project)| !project.trim().is_empty())
                    .collect();
                let dashboard = dashboard::Dashboard::new(
                    target.network,
                    target.configs.clone(),
                    target.deployments_dir.clone(),
                    args.services,
                    gcp_projects,
                    args.gcp_region.filter(|region| !region.trim().is_empty()),
                );
                running.spawn(dashboard::serve(dashboard, address));
            }
            if local {
                // Each chain is deployed to and driven on its own, so a slow
                // one holds up nobody else.
                for (key, config) in evm_chains(&target) {
                    let artifacts = args.artifacts.clone();
                    let file = Deployments::file_in(&target.deployments_dir, &key);
                    let seconds_per_block = args.seconds_per_block;
                    running.spawn(async move {
                        let client = Client::connect(&config).await?;
                        let deployments = deploy::ensure(&client, &key, &artifacts, &file).await?;
                        tracing::info!(chain = %key, label = config.label, "the chain is up");
                        market::run(&client, &deployments, seconds_per_block)
                            .await
                            .map_err(|error| error.context(format!("the market on {key}")))
                    });
                }
            } else {
                tracing::info!(network = %target.network, "public chains: nothing to drive, only to show");
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

/// `name=value`, for the arguments that pair the two.
fn name_and_value(text: &str) -> Result<(String, String), String> {
    text.split_once('=')
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .ok_or_else(|| format!("{text:?} is not name=value"))
}
