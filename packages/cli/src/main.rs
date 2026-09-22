//! The `shonin` command: what a strategy's author uses.

use std::path::PathBuf;

use alloy::primitives::{Address, U256};
use anyhow::{Context, Result};
use chain::bindings::profiles::IAuthorProfiles;
use chain::bindings::usd::ILocalUsd;
use chain::bindings::vault::IVault;
use chain::{Client, Role};
use clap::{Parser, Subcommand};
use shared::model::registry::JobState;

#[derive(Parser)]
#[command(name = "shonin", about = "Publish and manage Shonin strategies")]
struct Args {
    #[arg(long, env = "SHONIN_RPC_URL", default_value = "http://localhost:19083")]
    rpc_url: String,
    #[arg(long, env = "SHONIN_API_URL", default_value = "http://localhost:19081")]
    api: String,
    #[arg(
        long,
        env = "SHONIN_DEPLOYMENTS",
        default_value = ".shonin/local/deployments.json"
    )]
    deployments: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a strategy's vault and submit it to be built.
    Publish {
        /// The directory holding the strategy and its manifest.
        project: PathBuf,
        /// Keep the source out of the store. It is still built and still
        /// reviewed; only the publishing of it stops.
        #[arg(long)]
        sealed: bool,
    },
    /// Pack a project the way the build service unpacks it, and write the
    /// archive to standard output.
    Pack {
        /// The directory holding the strategy and its manifest.
        project: PathBuf,
    },
    /// Say who you are. Nothing depends on it.
    Author {
        #[arg(long)]
        handle: String,
        #[arg(long, default_value = "")]
        blurb: String,
    },
    /// Stop a vault. The admin key only.
    Kill {
        /// The vault's address.
        vault: Address,
    },
    /// Put some of the development chain's dollars somewhere.
    Faucet {
        who: Address,
        #[arg(long, default_value_t = 1_000_000.0)]
        amount: f64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_target(false)
        .without_time()
        .init();

    let args = Args::parse();

    // Packing needs nothing but the files, so it does not wait for a chain.
    if let Command::Pack { project } = &args.command {
        use std::io::Write;
        let archive = shonin_cli::pack(project)?;
        std::io::stdout().write_all(&archive)?;
        return Ok(());
    }

    let chain = Client::connect(&args.rpc_url).await?;
    let deployments = chain
        .deployments(&args.deployments)
        .await
        .context("no deployment file; is the platform running?")?;

    match args.command {
        Command::Publish { project, sealed } => {
            let author = chain.account(Role::Author)?;
            println!("packing {}", project.display());
            let published =
                shonin_cli::publish(&chain, &deployments, &args.api, &project, author, sealed)
                    .await?;
            match published.outcome {
                JobState::Succeeded => {
                    println!("built and registered");
                    println!("  vault    {:?}", published.vault);
                    println!(
                        "  strategy {}/strategies/{}",
                        site(&args.api),
                        published.slug
                    );
                    println!("  queue    {}/queue/{}", site(&args.api), published.job_id);
                }
                JobState::Failed { reason } => {
                    println!("the build failed: {reason}");
                    println!("  queue    {}/queue/{}", site(&args.api), published.job_id);
                }
                other => println!("the build is {}", other.label()),
            }
        }
        Command::Pack { .. } => unreachable!("handled before the chain is needed"),
        Command::Author { handle, blurb } => {
            let author = chain.account(Role::Author)?;
            chain
                .send(
                    author,
                    deployments.profiles,
                    IAuthorProfiles::setProfileCall {
                        handle: handle.clone(),
                        blurb,
                        linkLabels: Vec::new(),
                        linkUrls: Vec::new(),
                    },
                )
                .await?;
            println!("{author:?} is now {handle}");
        }
        Command::Kill { vault } => {
            let admin = chain.account(Role::Admin)?;
            chain.send(admin, vault, IVault::killCall {}).await?;
            println!("{vault:?} is stopped; every position is closed and the cash is home");
        }
        Command::Faucet { who, amount } => {
            let admin = chain.account(Role::Admin)?;
            let units = U256::from((amount * 1_000_000.0) as u128);
            chain
                .send(
                    admin,
                    deployments.usd,
                    ILocalUsd::mintCall {
                        to: who,
                        amount: units,
                    },
                )
                .await?;
            println!("{who:?} has {amount} more");
        }
    }
    Ok(())
}

/// The web app sits beside the API, one port below it.
fn site(api: &str) -> String {
    match api.rsplit_once(':') {
        Some((host, port)) => match port.parse::<u16>() {
            Ok(port) => format!("{host}:{}", port - 1),
            Err(_) => api.to_string(),
        },
        None => api.to_string(),
    }
}
