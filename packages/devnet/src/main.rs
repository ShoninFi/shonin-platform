//! The development chain.
//!
//! It deploys the contracts, funds the accounts the platform's roles use,
//! moves the clock faster than real time, and plays the market the venue
//! quotes. Everything else on the platform reads the file it writes and talks
//! to the chain it prepared.

mod deploy;
mod market;
mod seed;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use chain::Client;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Deploys and drives the local development chain")]
struct Args {
    #[arg(long, env = "SHONIN_RPC_URL")]
    rpc_url: String,
    /// Where the deploy writes, and every service reads, the addresses.
    #[arg(long)]
    deployments: PathBuf,
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
    /// Deploy the contracts and write the addresses, then stop.
    Deploy,
    /// Publish the reference agents through the platform's own flows.
    Seed {
        /// Where the API is answering.
        #[arg(long)]
        api: String,
        /// Where the strategies to publish live.
        #[arg(long, env = "SHONIN_AGENTS_DIR")]
        agents: Option<PathBuf>,
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
    let client = Client::connect(&args.rpc_url).await?;
    tracing::info!(chain = client.chain_id, "the chain is up");

    match args.command {
        Some(Command::Deploy) => {
            deploy::ensure(&client, &args.artifacts, &args.deployments).await?;
            Ok(())
        }
        Some(Command::Seed { api, agents }) => {
            let deployments = client.wait_for_deployments(&args.deployments).await?;
            seed::run(&client, &deployments, &api, agents.as_deref(), &args.store).await
        }
        None => {
            let deployments = deploy::ensure(&client, &args.artifacts, &args.deployments).await?;
            if args.port != 0 {
                let address = SocketAddr::from(([127, 0, 0, 1], args.port));
                tokio::spawn(serve_status(address));
            }
            market::run(&client, &deployments, args.seconds_per_block).await
        }
    }
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
