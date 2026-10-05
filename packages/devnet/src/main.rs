//! The development chains, and the ops dashboard.
//!
//! On the local network it deploys the contracts to every EVM chain, funds
//! the accounts the platform's roles use, moves each chain's clock faster than
//! real time, and plays the market each venue quotes. Everything else on the
//! platform reads the files it writes and talks to the chains it prepared.
//!
//! On the testnets it deploys and seeds the same stand-in stack, signed by the
//! accounts the developer's mnemonic derives, and drives nothing: the chains
//! keep their own time. It never deploys to mainnet ([`MAINNET_MISSING`]).
//!
//! On any network it serves the ops dashboard on its port: what is running,
//! locally and in the cloud.

mod checker;
mod dashboard;
mod deploy;
mod devnet_keys;
mod fund;
mod market;
mod publish;
mod seed;
mod solana;
mod solana_seed;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Result, bail};
use chain::{
    ChainArgs, ChainConfig, ChainKey, ChainKind, Deployments, NetworkName, Target, connect_to,
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
    /// The built Solana programs and their keypairs, as `anchor build`
    /// leaves them.
    #[arg(
        long,
        env = "SHONIN_SOLANA_PROGRAMS",
        default_value = "contracts/solana/target/deploy"
    )]
    solana_programs: PathBuf,
    /// What `fund` tops each testnet role up to.
    #[arg(long, default_value = "config/funding.toml")]
    funding: PathBuf,
    /// Seconds of chain time each block advances.
    #[arg(long, default_value_t = 3_600)]
    seconds_per_block: u64,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Deploy the contracts to every EVM chain of the network, and on the
    /// local network the programs to its Solana cluster, and write the
    /// addresses, then stop. Local and testnet only.
    Deploy,
    /// Show the funder and each role's account on every EVM chain of the
    /// network, with what each holds for gas and what it is short, and the
    /// Solana deployer, roles and programs: what to fund before a testnet
    /// deploy.
    Accounts,
    /// Deploy the Confidential Space checker on every EVM chain of the
    /// network and propose it to the registry beside the development
    /// checker. Already proposed is done.
    ProposeChecker,
    /// Top every role up on every EVM testnet from the funder, to the amounts
    /// in the funding file; buys HYPE on HyperCore for HyperEVM. Safe to run
    /// again; testnet only.
    Fund,
    /// Print the vault chain's RPC (overrides applied) and its deployment
    /// file, one per line, for the tasks that shell out to other tools.
    Chain,
    /// Print every served chain's deployment as
    /// `~<prefix>SHONIN_DEPLOYMENTS_JSON=[...]`, to append to a "^~^"-delimited
    /// gcloud list. Testnet addresses are never committed, so this is how a
    /// cloud launch is handed them; mainnet's are in the image, so there it
    /// prints nothing.
    DeploymentEnv {
        #[arg(long, default_value = "")]
        prefix: String,
    },
    /// Write a Solana keypair derived from the testnet Solana mnemonic.
    SolanaKeypair {
        /// Which: the deployer, or the registry's own address.
        #[arg(long, value_parser = ["deployer", "program"])]
        role: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Write every program's devnet keypair, derived from the testnet Solana
    /// mnemonic, into a directory as `<name>-keypair.json`, and print each
    /// program as `<name> <address> <final|upgradeable>`.
    SolanaProgramKeypairs {
        #[arg(long)]
        out: PathBuf,
    },
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
        /// With `--agent`, then use its vault as the full seed's live vault
        /// is used: a deposit priced and a claim booked and paid.
        #[arg(long)]
        live: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(chain::log_filter())
        .with_target(false)
        .init();

    let args = Args::parse();
    let target = args.chain_args.resolve()?;
    let local = target.network == NetworkName::Local;
    // A vault chain named on the command line: the devnet deploys to and
    // drives that one alone.
    let one = args.chain_args.vault_chain.is_some();
    let changes_chains = matches!(
        args.command,
        Some(
            Command::Deploy
                | Command::Seed { .. }
                | Command::Accounts
                | Command::Fund
                | Command::ProposeChecker
        )
    );
    if target.network == NetworkName::Mainnet && changes_chains {
        bail!("{MAINNET_MISSING}");
    }

    match args.command {
        Some(Command::Deploy) => {
            let names = chain_names(&target);
            for (key, config) in only(&target, one, evm_chains(&target)) {
                let client = connect_to(target.network, &key, &config).await?;
                let file = Deployments::file_in(&target.deployments_dir, &key);
                deploy::ensure(
                    &client,
                    target.network,
                    &key,
                    &args.artifacts,
                    &file,
                    &names,
                )
                .await?;
            }
            if local {
                for (key, config) in only(&target, one, solana_clusters(&target)) {
                    let client = chain::connect_solana(target.network, &key, &config).await?;
                    let file = Deployments::file_in(&target.deployments_dir, &key);
                    solana::ensure(&client, &key, &config.rpc, &args.solana_programs, &file)
                        .await?;
                }
            }
            Ok(())
        }
        Some(Command::Accounts) => accounts(&target, &args.funding).await,
        Some(Command::ProposeChecker) => {
            checker::run(
                target.network,
                only(&target, one, evm_chains(&target)),
                &target.deployments_dir,
                &args.artifacts,
            )
            .await
        }
        Some(Command::Fund) => {
            fund::run(
                target.network,
                only(&target, one, evm_chains(&target)),
                &args.funding,
            )
            .await
        }
        Some(Command::DeploymentEnv { prefix }) => {
            match target.network {
                NetworkName::Mainnet => {}
                NetworkName::Local => bail!("the cloud never runs against local chains"),
                NetworkName::Testnet => {
                    let mut all = Vec::new();
                    for (key, _) in evm_chains(&target) {
                        let file = Deployments::file_in(&target.deployments_dir, &key);
                        let text = std::fs::read_to_string(&file).map_err(|error| {
                            anyhow::anyhow!(
                                "no testnet deployment at {} ({error}): run task chains:deploy NETWORK=testnet first",
                                file.display()
                            )
                        })?;
                        all.push(serde_json::to_value(Deployments::from_json(&text)?)?);
                    }
                    // A Solana cluster's file, where its programs are
                    // deployed, named by its key so a reader can find it.
                    for (key, _) in solana_clusters(&target) {
                        let file = Deployments::file_in(&target.deployments_dir, &key);
                        if let Ok(text) = std::fs::read_to_string(&file) {
                            let mut solana = chain::SolanaDeployments::from_json(&text)?;
                            if solana.chain_key.is_empty() {
                                solana.chain_key = key.to_string();
                            }
                            all.push(serde_json::to_value(solana)?);
                        }
                    }
                    print!(
                        "~{prefix}{}={}",
                        chain::args::DEPLOYMENTS_JSON,
                        serde_json::to_string(&all)?
                    );
                }
            }
            Ok(())
        }
        Some(Command::Chain) => {
            println!("{}", target.config.first_rpc());
            println!("{}", target.deployments_file().display());
            Ok(())
        }
        Some(Command::SolanaKeypair { role, out }) => {
            let index = if role == "program" {
                solana::PROGRAM
            } else {
                solana::DEPLOYER
            };
            let pair = solana::keypair_from_mnemonic(&solana::testnet_mnemonic()?, index)?;
            chain::solana::keys::write_keypair(&pair, &out)?;
            println!("{}", chain::solana::Signer::pubkey(&pair));
            Ok(())
        }
        Some(Command::SolanaProgramKeypairs { out }) => {
            let written = devnet_keys::write_program_keypairs(&solana::testnet_mnemonic()?, &out)?;
            for (name, address) in written {
                let kind = if solana::deployed_final(name) {
                    "final"
                } else {
                    "upgradeable"
                };
                println!("{name} {address} {kind}");
            }
            Ok(())
        }
        Some(Command::Seed { .. }) if target.key.kind == ChainKind::Solana => {
            let served = target.serve_solana().await?;
            solana_seed::run(&served).await
        }
        Some(Command::Seed {
            api,
            agents,
            agent,
            live,
        }) => {
            let client = target.connect().await?;
            let deployments = target.wait_for_deployments(&client).await?;
            match agent {
                Some(name) => {
                    seed::one(
                        &client,
                        target.vault_chain,
                        &deployments,
                        &api,
                        agents.as_deref(),
                        &name,
                        live,
                    )
                    .await
                }
                None => {
                    seed::run(
                        &client,
                        target.vault_chain,
                        &deployments,
                        &api,
                        agents.as_deref(),
                    )
                    .await
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
                let names = chain_names(&target);
                for (key, config) in only(&target, one, evm_chains(&target)) {
                    let names = names.clone();
                    let artifacts = args.artifacts.clone();
                    let file = Deployments::file_in(&target.deployments_dir, &key);
                    let seconds_per_block = args.seconds_per_block;
                    let network = target.network;
                    running.spawn(async move {
                        let client = connect_to(network, &key, &config).await?;
                        let deployments =
                            deploy::ensure(&client, network, &key, &artifacts, &file, &names)
                                .await?;
                        tracing::info!(chain = %key, label = config.label, "the chain is up");
                        market::run(&client, &deployments, seconds_per_block)
                            .await
                            .map_err(|error| error.context(format!("the market on {key}")))
                    });
                }
                for (key, config) in only(&target, one, solana_clusters(&target)) {
                    let programs = args.solana_programs.clone();
                    let file = Deployments::file_in(&target.deployments_dir, &key);
                    let seconds_per_block = args.seconds_per_block;
                    let network = target.network;
                    // A cluster that can't be brought up is said, and the
                    // EVM chains carry on: Solana needs tools of its own.
                    running.spawn(async move {
                        let driven = async {
                            let client = chain::connect_solana(network, &key, &config).await?;
                            let deployments =
                                solana::ensure(&client, &key, &config.rpc, &programs, &file)
                                    .await?;
                            tracing::info!(chain = %key, label = config.label, "the cluster is up");
                            solana::run_market(&client, &deployments, seconds_per_block).await
                        };
                        if let Err(error) = driven.await {
                            tracing::error!(chain = %key, error = format!("{error:#}"), "the Solana cluster is not served");
                        }
                        Ok(())
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

/// Why the devnet never deploys to mainnet: what it deploys is the stand-in
/// stack, which is for local chains and testnets.
pub const MAINNET_MISSING: &str = "The devnet does not deploy to mainnet. What it deploys is the \
stand-in stack: a dollar anyone may mint, a stand-in venue, and an attestation verifier that trusts \
one development key. It is for local chains and testnets. Mainnet addresses are committed under \
config/deployments/mainnet.";

/// Each role's account on every EVM chain of the network, with its gas, and
/// on the testnets the funder, what each role is short of its amount in the
/// funding file, and the Solana deployer, roles and programs.
async fn accounts(target: &Target, funding_file: &std::path::Path) -> Result<()> {
    use alloy::primitives::utils::format_ether;
    let testnet = target.network == NetworkName::Testnet;
    let funding = if testnet {
        Some(fund::Funding::read(funding_file)?)
    } else {
        None
    };
    let funder = if testnet {
        Some(fund::funder()?.address())
    } else {
        None
    };
    for (key, config) in evm_chains(target) {
        let client = connect_to(target.network, &key, &config).await?;
        println!("{} ({key})", config.label);
        match (&funding, funder) {
            (Some(funding), Some(funder)) => {
                let short = fund::shortfalls(funding, &client, &key).await?;
                let reserve = funding.reserve(&key).unwrap_or_default();
                let held = client.balance(funder).await?;
                let needs = (short.total + reserve).saturating_sub(held);
                println!(
                    "  {:<9} {funder:?}  {}  (the roles are short {}, the reserve is {}{})",
                    "FUNDER",
                    format_ether(held),
                    format_ether(short.total),
                    format_ether(reserve),
                    if needs.is_zero() {
                        String::new()
                    } else {
                        format!("; it needs {} more", format_ether(needs))
                    }
                );
                if let Some(api) = hyperliquid::Api::for_chain(client.chain_id) {
                    let perp = api.perp_withdrawable(funder).await.unwrap_or(0.0);
                    let usdc = api.spot_balance(funder, "USDC").await.unwrap_or(0.0);
                    let hype = api.spot_balance(funder, "HYPE").await.unwrap_or(0.0);
                    println!(
                        "  {:<9} on HyperCore: ${perp} perp, ${usdc} spot, {hype} HYPE (chains:fund buys HYPE with it)",
                        ""
                    );
                }
                for (role, address, balance, need) in short.rows {
                    println!(
                        "  {:<9} {address:?}  {}{}",
                        role.label(),
                        format_ether(balance),
                        if need.is_zero() {
                            String::new()
                        } else {
                            format!("  (short {})", format_ether(need))
                        }
                    );
                }
            }
            _ => {
                for role in fund::ROLES {
                    let address = client.account(role)?;
                    let balance = client.balance(address).await?;
                    println!(
                        "  {:<9} {address:?}  {}",
                        role.label(),
                        format_ether(balance)
                    );
                }
            }
        }
    }
    if testnet {
        let phrase = solana::testnet_mnemonic()?;
        let deployer = solana::keypair_from_mnemonic(&phrase, solana::DEPLOYER)?;
        println!("Solana devnet");
        println!(
            "  {:<20} {}  (the Solana funder: it pays for the programs and funds the roles)",
            "FUNDER (deployer)",
            chain::solana::Signer::pubkey(&deployer)
        );
        for role in fund::ROLES {
            let index = chain::solana::keys::ROLE_BASE + role.account_index() as u32;
            let pair = solana::keypair_from_mnemonic(&phrase, index)?;
            println!(
                "  {:<20} {}",
                role.label(),
                chain::solana::Signer::pubkey(&pair)
            );
        }
        for (name, index) in devnet_keys::PROGRAM_KEYS {
            let program = solana::keypair_from_mnemonic(&phrase, index)?;
            println!("  {name:<20} {}", chain::solana::Signer::pubkey(&program));
        }
    }
    Ok(())
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

/// The chains of `chains` the devnet works on: every one, or with a vault
/// chain named, only the chain it is.
fn only(
    target: &Target,
    one: bool,
    chains: Vec<(ChainKey, ChainConfig)>,
) -> Vec<(ChainKey, ChainConfig)> {
    chains
        .into_iter()
        .filter(|(key, _)| !one || key.same_chain(&target.key))
        .collect()
}

/// Every Solana cluster of the network.
fn solana_clusters(target: &Target) -> Vec<(ChainKey, ChainConfig)> {
    let Some(network) = target.configs.network(target.network) else {
        return Vec::new();
    };
    let mut clusters: Vec<(ChainKey, ChainConfig)> = network
        .iter()
        .filter(|(_, key)| key.kind == ChainKind::Solana)
        .filter_map(|(_, key)| Some((key.clone(), target.configs.get(key)?.clone())))
        .collect();
    clusters.sort_by(|a, b| a.0.cmp(&b.0));
    clusters.dedup_by(|a, b| a.0 == b.0);
    clusters
}

/// Each EVM chain of the network by the name `shonin.toml` uses and its chain
/// id, which is how a vault factory finds the chains a strategy trades on.
fn chain_names(target: &Target) -> Vec<(String, u64)> {
    let Some(network) = target.configs.network(target.network) else {
        return Vec::new();
    };
    network
        .iter()
        .filter_map(|(chain, key)| key.evm_chain_id().map(|id| (chain.name().to_string(), id)))
        .collect()
}

/// `name=value`, for the arguments that pair the two.
fn name_and_value(text: &str) -> Result<(String, String), String> {
    text.split_once('=')
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .ok_or_else(|| format!("{text:?} is not name=value"))
}
